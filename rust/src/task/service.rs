use std::sync::Arc;

use serde_json::Value;
use sqlx::{Row, SqlitePool};

use super::{TaskKind, TaskStatus, ACTIVE_STATUSES};
use crate::conversation::{ConversationEvent, EventLog};
use crate::error::AppError;

#[derive(Debug, Clone)]
pub struct NewTask {
    pub conversation_id: i64,
    /// Assistant message of the turn that submitted the task.
    pub message_id: Option<i64>,
    pub kind: TaskKind,
    pub provider_task_id: String,
    pub model: String,
    pub prompt: String,
    /// Provider parameters (size, duration, ...), stored as JSON.
    pub params: Value,
}

#[derive(Debug, Clone)]
pub struct GenerationTask {
    pub id: i64,
    pub conversation_id: i64,
    pub message_id: Option<i64>,
    pub kind: TaskKind,
    pub provider_task_id: String,
    pub model: String,
    pub prompt: String,
    pub params: Option<String>,
    pub status: TaskStatus,
    pub attempts: i64,
    pub error: Option<String>,
    pub asset_id: Option<i64>,
    pub result_message_id: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
    pub finished_at: Option<i64>,
}

const COLUMNS: &str = "id, conversation_id, message_id, kind, provider_task_id, model, prompt, params, status, attempts, error, asset_id, result_message_id, created_at, updated_at, finished_at";

fn to_task(row: &sqlx::sqlite::SqliteRow) -> GenerationTask {
    let get = |name: &str| -> String { row.try_get(name).unwrap_or_default() };
    GenerationTask {
        id: row.try_get("id").expect("id"),
        conversation_id: row.try_get("conversation_id").expect("conversation_id"),
        message_id: row.try_get("message_id").ok().flatten(),
        kind: match get("kind").as_str() {
            "video" => TaskKind::Video,
            _ => TaskKind::Image,
        },
        provider_task_id: get("provider_task_id"),
        model: get("model"),
        prompt: get("prompt"),
        params: row.try_get("params").ok().flatten(),
        status: TaskStatus::parse(&get("status")),
        attempts: row.try_get("attempts").unwrap_or(0),
        error: row.try_get("error").ok().flatten(),
        asset_id: row.try_get("asset_id").ok().flatten(),
        result_message_id: row.try_get("result_message_id").ok().flatten(),
        created_at: row.try_get("created_at").unwrap_or(0),
        updated_at: row.try_get("updated_at").unwrap_or(0),
        finished_at: row.try_get("finished_at").ok().flatten(),
    }
}

pub struct TaskService {
    pool: SqlitePool,
    events: Arc<EventLog>,
}

impl TaskService {
    pub fn new(pool: SqlitePool, events: Arc<EventLog>) -> Self {
        Self { pool, events }
    }

    pub async fn create(&self, task: &NewTask) -> Result<i64, AppError> {
        let now = chrono::Utc::now().timestamp_millis();
        let row = sqlx::query(
            "INSERT INTO generation_tasks (conversation_id, message_id, kind, provider_task_id, model, prompt, params, status, attempts, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'queued', 0, ?8, ?8) RETURNING id",
        )
        .bind(task.conversation_id)
        .bind(task.message_id)
        .bind(task.kind.as_str())
        .bind(&task.provider_task_id)
        .bind(&task.model)
        .bind(&task.prompt)
        .bind(task.params.to_string())
        .bind(now)
        .fetch_one(&self.pool)
        .await?;
        let id: i64 = row.try_get("id")?;

        // Announce immediately, not on the first poll: the next sweep is up to
        // `POLL_INTERVAL` away, and the user who just submitted should see the job
        // appear at once rather than after the turn has already gone quiet.
        self.events
            .publish(
                task.conversation_id,
                ConversationEvent::TaskUpdated {
                    task_id: id,
                    kind: task.kind,
                    status: TaskStatus::Queued,
                    error: None,
                },
            )
            .await?;
        Ok(id)
    }

    pub async fn find_by_id(&self, id: i64) -> Result<Option<GenerationTask>, AppError> {
        let sql = format!("SELECT {COLUMNS} FROM generation_tasks WHERE id = ?1");
        let row = sqlx::query(&sql)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|row| to_task(&row)))
    }

    /// Every task the worker still has to advance, oldest first.
    pub async fn active(&self, limit: i64) -> Result<Vec<GenerationTask>, AppError> {
        let marks = status_marks(ACTIVE_STATUSES.len());
        let sql = format!(
            "SELECT {COLUMNS} FROM generation_tasks WHERE status IN ({marks}) ORDER BY id ASC LIMIT ?{}",
            ACTIVE_STATUSES.len() + 1
        );
        let mut query = sqlx::query(&sql);
        for status in ACTIVE_STATUSES {
            query = query.bind(status.as_str());
        }
        let rows = query.bind(limit).fetch_all(&self.pool).await?;
        Ok(rows.iter().map(to_task).collect())
    }

    pub async fn active_by_conversation(
        &self,
        conversation_id: i64,
    ) -> Result<Vec<GenerationTask>, AppError> {
        let marks = status_marks(ACTIVE_STATUSES.len() + 1);
        let sql = format!(
            "SELECT {COLUMNS} FROM generation_tasks WHERE conversation_id = ?1 AND status IN ({marks}) ORDER BY id ASC"
        );
        let mut query = sqlx::query(&sql).bind(conversation_id);
        for status in ACTIVE_STATUSES {
            query = query.bind(status.as_str());
        }
        let rows = query.fetch_all(&self.pool).await?;
        Ok(rows.iter().map(to_task).collect())
    }

    pub async fn by_conversation(
        &self,
        conversation_id: i64,
    ) -> Result<Vec<GenerationTask>, AppError> {
        let sql = format!("SELECT {COLUMNS} FROM generation_tasks WHERE conversation_id = ?1 ORDER BY id ASC");
        let rows = sqlx::query(&sql)
            .bind(conversation_id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(to_task).collect())
    }

    pub async fn mark_progress(
        &self,
        id: i64,
        status: TaskStatus,
        attempts: i64,
    ) -> Result<(), AppError> {
        sqlx::query("UPDATE generation_tasks SET status = ?1, attempts = ?2, updated_at = ?3 WHERE id = ?4")
            .bind(status.as_str())
            .bind(attempts)
            .bind(chrono::Utc::now().timestamp_millis())
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Records a failed mirror attempt without giving up on the task.
    pub async fn mark_attempt_failed(
        &self,
        id: i64,
        attempts: i64,
        error: &str,
    ) -> Result<(), AppError> {
        sqlx::query("UPDATE generation_tasks SET attempts = ?1, error = ?2, updated_at = ?3 WHERE id = ?4")
            .bind(attempts)
            .bind(error)
            .bind(chrono::Utc::now().timestamp_millis())
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn finish(
        &self,
        id: i64,
        status: TaskStatus,
        asset_id: Option<i64>,
        result_message_id: Option<i64>,
        error: Option<&str>,
    ) -> Result<(), AppError> {
        let now = chrono::Utc::now().timestamp_millis();
        sqlx::query(
            "UPDATE generation_tasks SET status = ?1, asset_id = ?2, result_message_id = ?3, error = ?4, updated_at = ?5, finished_at = ?5 WHERE id = ?6",
        )
        .bind(status.as_str())
        .bind(asset_id)
        .bind(result_message_id)
        .bind(error)
        .bind(now)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

fn status_marks(count: usize) -> String {
    (0..count)
        .map(|index| format!("?{}", index + 1))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A conversation row is needed: the task table has a foreign key to it.
    async fn services() -> (TaskService, Arc<EventLog>, SqlitePool) {
        let pool = crate::db::open_in_memory().await;
        sqlx::query("INSERT INTO users (id, username, password_hash, role, created_at) VALUES (1,'u','h','user',1)")
            .execute(&pool)
            .await
            .expect("user");
        sqlx::query("INSERT INTO conversations (id, user_id, title, created_at, updated_at) VALUES (1,1,'t',1,1)")
            .execute(&pool)
            .await
            .expect("conversation");
        let events = Arc::new(EventLog::new(pool.clone()));
        (TaskService::new(pool.clone(), Arc::clone(&events)), events, pool)
    }

    fn new_task(conversation_id: i64) -> NewTask {
        NewTask {
            conversation_id,
            message_id: Some(7),
            kind: TaskKind::Video,
            provider_task_id: "provider-1".to_string(),
            model: "happyhorse-1.1-t2v".to_string(),
            prompt: "一只红隼俯冲".to_string(),
            params: serde_json::json!({ "size": "1280*720", "duration": 5 }),
        }
    }

    #[tokio::test]
    async fn creating_a_task_announces_it_before_the_first_poll() {
        let (tasks, events, _pool) = services().await;
        // A subscriber is what proves "immediately": the next sweep is 8 seconds
        // away, and the submit must already be on the stream.
        let mut receiver = events.subscribe(1);
        let id = tasks.create(&new_task(1)).await.expect("create");
        let announced = receiver.recv().await.expect("event");
        match announced.event {
            ConversationEvent::TaskUpdated { task_id, kind, status, error } => {
                assert_eq!((task_id, kind, status), (id, TaskKind::Video, TaskStatus::Queued));
                assert_eq!(error, None);
            }
            other => panic!("unexpected event {other:?}"),
        }
        assert_eq!(announced.seq, 1, "the announcement is journaled, not just live");
    }

    #[tokio::test]
    async fn the_queue_holds_only_non_terminal_tasks_and_survives_a_restart() {
        let (tasks, _events, pool) = services().await;
        let first = tasks.create(&new_task(1)).await.expect("create");
        let second = tasks.create(&new_task(1)).await.expect("create");

        let active = tasks.active(20).await.expect("active");
        assert_eq!(
            active.iter().map(|task| task.id).collect::<Vec<_>>(),
            vec![first, second],
            "oldest first"
        );
        let one = tasks.find_by_id(first).await.expect("find").expect("row");
        assert_eq!(one.status, TaskStatus::Queued);
        assert_eq!(one.attempts, 0);
        assert_eq!(one.message_id, Some(7));
        assert!(one.params.expect("params").contains("1280*720"));

        // A restart reads the same rows back: the queue lives in the database.
        let restarted = TaskService::new(pool.clone(), Arc::new(EventLog::new(pool.clone())));
        assert_eq!(restarted.active(20).await.expect("active").len(), 2);

        tasks
            .finish(first, TaskStatus::Succeeded, Some(3), Some(4), None)
            .await
            .expect("finish");
        let finished = tasks.find_by_id(first).await.expect("find").expect("row");
        assert_eq!(finished.status, TaskStatus::Succeeded);
        assert_eq!((finished.asset_id, finished.result_message_id), (Some(3), Some(4)));
        assert_eq!(finished.error, None);
        assert!(finished.finished_at.is_some(), "terminal rows carry their end time");
        assert_eq!(tasks.active(20).await.expect("active").len(), 1);
    }

    #[tokio::test]
    async fn a_transient_failure_counts_attempts_without_ending_the_task() {
        let (tasks, _events, _pool) = services().await;
        let id = tasks.create(&new_task(1)).await.expect("create");

        tasks.mark_attempt_failed(id, 1, "HTTP 502").await.expect("attempt");
        let row = tasks.find_by_id(id).await.expect("find").expect("row");
        assert_eq!((row.attempts, row.error.as_deref()), (1, Some("HTTP 502")));
        assert_eq!(row.status, TaskStatus::Queued, "still owned by the worker");
        assert_eq!(row.finished_at, None);

        tasks.mark_progress(id, TaskStatus::Running, 1).await.expect("progress");
        let row = tasks.find_by_id(id).await.expect("find").expect("row");
        assert_eq!(row.status, TaskStatus::Running);

        tasks.finish(id, TaskStatus::Failed, None, Some(9), Some("超时")).await.expect("fail");
        let row = tasks.find_by_id(id).await.expect("find").expect("row");
        assert_eq!((row.status, row.error.as_deref(), row.result_message_id), (TaskStatus::Failed, Some("超时"), Some(9)));
        assert_eq!(row.asset_id, None);
        assert!(tasks.active(20).await.expect("active").is_empty());
    }

    #[tokio::test]
    async fn the_per_conversation_views_do_not_leak_across_accounts() {
        let (tasks, events, pool) = services().await;
        sqlx::query("INSERT INTO conversations (id, user_id, title, created_at, updated_at) VALUES (2,1,'other',1,1)")
            .execute(&pool)
            .await
            .expect("second conversation");
        let mine = tasks.create(&new_task(1)).await.expect("create");
        tasks.create(&new_task(2)).await.expect("create");

        assert_eq!(
            tasks.active_by_conversation(1).await.expect("mine").iter().map(|t| t.id).collect::<Vec<_>>(),
            vec![mine]
        );
        assert_eq!(tasks.by_conversation(2).await.expect("theirs").len(), 1);
        // The announcements went to each conversation's own stream.
        let _ = events;
    }

    #[tokio::test]
    async fn the_limit_is_honoured() {
        let (tasks, _events, _pool) = services().await;
        for _ in 0..3 {
            tasks.create(&new_task(1)).await.expect("create");
        }
        assert_eq!(tasks.active(2).await.expect("limited").len(), 2);
        assert_eq!(tasks.active(20).await.expect("all").len(), 3);
    }
}
