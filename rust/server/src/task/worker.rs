use std::time::Duration;

use crate::agent::mode::Mode;
use crate::error::{describe_error, AppError};
use crate::media::service::VideoTaskState;
use crate::state::AppState;

use super::service::GenerationTask;
use super::{TaskKind, TaskStatus};

/// Provider renders are minutes long; polling faster only burns quota.
pub const POLL_INTERVAL: Duration = Duration::from_secs(8);

/// Give up on a task the provider never resolves.
const MAX_TASK_AGE: Duration = Duration::from_secs(20 * 60);

/// The provider URL is short-lived, so a failed mirror is retried on the next
/// ticks rather than dropping the render on the floor.
const MAX_MIRROR_ATTEMPTS: i64 = 3;

/// Every task enumerated per sweep.
const SWEEP_LIMIT: i64 = 20;

/// A completed or failed render is announced in the mode it belongs to.
const RESULT_MODE: Mode = Mode::Video;

/// Advances non-terminal `generation_tasks` rows on a timer.
///
/// The Nest version drove this from an interval callback and needed a `sweeping`
/// flag to keep two callbacks from overlapping. Here the loop awaits each sweep,
/// so a slow poll delays the next tick instead of racing it, and the flag is gone.
pub struct TaskWorker {
    state: AppState,
}

impl TaskWorker {
    pub fn new(state: AppState) -> Self {
        Self { state }
    }

    /// The timer loop; `sweep` is public so a test can drive it directly.
    pub async fn run(&self) {
        let mut ticker = tokio::time::interval(POLL_INTERVAL);
        // The first tick fires immediately: tasks survive a restart in the
        // database, so picking them up at boot is the point, not a bonus.
        loop {
            ticker.tick().await;
            self.sweep().await;
        }
    }

    /// Advances every non-terminal task by one poll. Tasks live in the database,
    /// so a restart resumes them on the next sweep instead of losing them.
    pub async fn sweep(&self) {
        let pending = match self.state.tasks.active(SWEEP_LIMIT).await {
            Ok(pending) => pending,
            Err(error) => {
                // Enumerating the queue failed: nothing to iterate.
                tracing::warn!(%error, "task sweep failed");
                return;
            }
        };
        for task in pending {
            if let Err(error) = self.advance(&task).await {
                // Per task, not per sweep. Provider errors are handled inside
                // advance(), so anything arriving here is an unexpected write or
                // plumbing failure — and one such task must not stop the rest of
                // the queue, or a task that always fails would starve every task
                // behind it until someone notices.
                tracing::warn!(task = task.id, %error, "task could not be advanced");
            }
        }
    }

    async fn advance(&self, task: &GenerationTask) -> Result<(), AppError> {
        let age = chrono::Utc::now().timestamp_millis() - task.created_at;
        if age > MAX_TASK_AGE.as_millis() as i64 {
            return self
                .fail(
                    task,
                    &format!(
                        "任务超时（超过 {} 分钟未完成）",
                        MAX_TASK_AGE.as_secs() / 60
                    ),
                )
                .await;
        }

        let api_key = self.state.require_key().await?;
        let snapshot = match self
            .state
            .media
            .query_video_task(&api_key, &task.provider_task_id)
            .await
        {
            Ok(snapshot) => snapshot,
            Err(error) => {
                // Transient failures are expected across a multi-minute render:
                // count them and keep polling until the age limit trips.
                let attempts = task.attempts + 1;
                let reason = describe_error(&error);
                self.state
                    .tasks
                    .mark_attempt_failed(task.id, attempts, &reason)
                    .await?;
                // The status stays whatever the row already said: a transient poll
                // failure is not a state change.
                self.publish(task, task.status, Some(&reason))
                    .await?;
                return Ok(());
            }
        };

        match snapshot.state {
            VideoTaskState::Pending | VideoTaskState::Running => {
                let status = if snapshot.state == VideoTaskState::Running {
                    TaskStatus::Running
                } else {
                    TaskStatus::Queued
                };
                self.state
                    .tasks
                    .mark_progress(task.id, status, task.attempts)
                    .await?;
                self.publish(task, status, None).await?;
                Ok(())
            }
            VideoTaskState::Failed => {
                let reason = snapshot
                    .message
                    .clone()
                    .unwrap_or_else(|| format!("供应商返回状态 {}", snapshot.raw_status));
                self.fail(task, &reason).await
            }
            VideoTaskState::Succeeded => {
                let url = snapshot
                    .video_url
                    .clone()
                    .expect("SUCCEEDED without a URL normalizes to failed");
                self.mirror(task, &url).await
            }
        }
    }

    /// Downloads the render at the moment the provider reports success. This is
    /// the only window in which the URL works — it is not a durable reference.
    async fn mirror(&self, task: &GenerationTask, video_url: &str) -> Result<(), AppError> {
        let stored = match self.state.media.store(TaskKind::Video, video_url).await {
            Ok(stored) => stored,
            Err(error) => {
                let attempts = task.attempts + 1;
                let reason = describe_error(&error);
                if attempts < MAX_MIRROR_ATTEMPTS {
                    self.state
                        .tasks
                        .mark_attempt_failed(task.id, attempts, &reason)
                        .await?;
                    self.publish(task, task.status, Some(&reason)).await?;
                    return Ok(());
                }
                return self.fail(task, &format!("结果下载失败：{reason}")).await;
            }
        };

        let message_id = self
            .state
            .conversations
            .append_assistant_message(
                task.conversation_id,
                &format!("视频生成完成。\n\n提示词：{}", task.prompt),
                Some(RESULT_MODE),
            )
            .await?;
        let asset_id = self.state.conversations.add_asset(message_id, &stored).await?;
        self.state
            .tasks
            .finish(task.id, TaskStatus::Succeeded, Some(asset_id), Some(message_id), None)
            .await?;
        self.publish(task, TaskStatus::Succeeded, None).await?;
        self.publish_message(task.conversation_id, message_id).await
    }

    /// The failure is written into the conversation rather than only pushed: a
    /// user who was offline when it happened must still find out why.
    async fn fail(&self, task: &GenerationTask, reason: &str) -> Result<(), AppError> {
        let message_id = self
            .state
            .conversations
            .append_assistant_message(
                task.conversation_id,
                &format!("视频生成失败：{reason}\n\n提示词：{}", task.prompt),
                Some(RESULT_MODE),
            )
            .await?;
        self.state
            .tasks
            .finish(task.id, TaskStatus::Failed, None, Some(message_id), Some(reason))
            .await?;
        self.publish(task, TaskStatus::Failed, Some(reason)).await?;
        self.publish_message(task.conversation_id, message_id).await
    }

    async fn publish(
        &self,
        task: &GenerationTask,
        status: TaskStatus,
        error: Option<&str>,
    ) -> Result<(), AppError> {
        self.state
            .events
            .publish(
                task.conversation_id,
                crate::conversation::ConversationEvent::TaskUpdated {
                    task_id: task.id,
                    kind: task.kind,
                    status,
                    error: error.map(|value| value.to_string()),
                },
            )
            .await
            .map(|_| ())
    }

    async fn publish_message(&self, conversation_id: i64, message_id: i64) -> Result<(), AppError> {
        if let Some(view) = self.state.conversations.message_view(message_id).await? {
            self.state
                .events
                .publish(
                    conversation_id,
                    crate::conversation::ConversationEvent::MessageAdded { message: view },
                )
                .await
                .map(|_| ())?;
        }
        Ok(())
    }
}
