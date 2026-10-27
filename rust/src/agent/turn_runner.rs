use crate::agent::mode::Mode;
use crate::conversation::ConversationEvent;
use crate::error::AppError;
use crate::state::AppState;

use super::service::AgentService;

/// Owns the turns running in this process: one per conversation, detached from
/// the request that submitted it.
///
/// `POST /api/v1/conversations/:id/turns` answers 202 as soon as the rows exist
/// and the run is in flight. Two decisions ride on that detachment:
///
/// - A dropped connection no longer aborts the turn. Closing the page mid-turn
///   used to cancel the paid generation with the request; now the turn finishes
///   server-side and a reconnect catches up on the stream.
/// - One running turn per conversation, answered with 409 on the second attempt:
///   the old guard was the single POST connection; with send and subscribe split,
///   nothing else stops a double submit from billing twice.
pub struct TurnRunner {
    state: AppState,
}

impl TurnRunner {
    pub fn new(state: AppState) -> Self {
        Self { state }
    }

    pub fn is_running(&self, conversation_id: i64) -> bool {
        self.state.is_turn_running(conversation_id)
    }

    /// Where a stream connection with no client cursor starts.
    ///
    /// A conversation whose turn is still running gets its cursor pushed back to
    /// just before that turn's first journal event — SSR deliberately does not
    /// paint an unfinished assistant row, so the replay must carry the turn from
    /// its beginning. Otherwise the cursor is the newest event: everything older is
    /// already in the rows the page was painted from.
    pub async fn catch_up_cursor(&self, conversation_id: i64) -> Result<i64, AppError> {
        if let Some(unfinished) = self
            .state
            .conversations
            .unfinished_assistant(conversation_id)
            .await?
        {
            if self.is_running(conversation_id) {
                return Ok(self
                    .state
                    .events
                    .seq_before_turn(conversation_id, unfinished)
                    .await?);
            }
        }
        Ok(self.state.events.latest_seq(conversation_id).await?)
    }

    /// Starts a turn and returns its assistant message id — the turn's identity,
    /// which the stream also calls it by (`turn_started`, deltas, assets). Returns
    /// None when a turn is already running for that conversation.
    ///
    /// Everything durable happens synchronously before returning: the submit
    /// endpoint must not answer 202 and then have the turn's rows appear "later",
    /// and the journal order user_message -> turn_started -> deltas is what
    /// replaying clients depend on.
    pub async fn start(
        &self,
        conversation_id: i64,
        prompt: &str,
        mode: Mode,
    ) -> Result<Option<i64>, AppError> {
        if !self.state.claim_turn(conversation_id) {
            return Ok(None);
        }

        let user_message_id = self
            .state
            .conversations
            .append_user_message(conversation_id, prompt, Some(mode))
            .await?;
        let message_id = self
            .state
            .conversations
            .create_assistant_placeholder(conversation_id, Some(mode))
            .await?;

        if let Some(view) = self.state.conversations.message_view(user_message_id).await? {
            self.state
                .events
                .publish(conversation_id, ConversationEvent::UserMessage { message: view })
                .await?;
        }
        self.state
            .events
            .publish(
                conversation_id,
                ConversationEvent::TurnStarted { message_id, mode: Some(mode) },
            )
            .await?;

        let state = self.state.clone();
        let agent = AgentService::new(state.clone());
        tokio::spawn(async move {
            // `AgentService::run` finalizes in every path it can reach. Should this
            // task ever be cancelled mid-flight, the slot is still released and the
            // boot recovery (`abandon_stale_placeholders`) ends the row.
            agent.run(conversation_id, message_id, mode).await;
            state.release_turn(conversation_id);
            if let Err(error) = state.events.prune(conversation_id).await {
                tracing::warn!(%error, "journal could not be pruned");
            }
        });

        Ok(Some(message_id))
    }
}
