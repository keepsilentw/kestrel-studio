use std::collections::HashMap;
use std::sync::Mutex;

use sqlx::{Row, SqlitePool};
use tokio::sync::broadcast;

use crate::error::AppError;

// The event itself is a wire type and lives in `kestrel-proto` with the payload
// structs it carries, so the browser parses exactly what is journalled.
// `PublishedEvent` below stays here: it is the journal row's envelope, not part of
// the contract.
pub use kestrel_proto::event::{ConversationEvent, MessageView};

/// How many recent events a conversation's journal keeps. Sized above the delta
/// count of one ordinary turn (docs/verification.md §1: `high` reasoning effort
/// produced >1300 deltas), so a brief disconnect replays seamlessly and only
/// long-stale cursors fall back to `resync`.
const RETAINED_EVENTS: i64 = 2000;

/// An event together with the journal id that becomes its SSE `id:` field.
#[derive(Debug, Clone)]
pub struct PublishedEvent {
    pub seq: i64,
    pub event: ConversationEvent,
    /// The exact bytes stored in the journal's `payload` column and handed to the
    /// `data:` field of the frame. Carried alongside the parsed event so a replay
    /// is byte-equivalent to the live frame — field order included — and stays so
    /// even after the event shape changes under an older row.
    pub payload: String,
}

/// Durable event log plus live fan-out for one conversation's stream — the single
/// source of order for everything the browser (or any future client) watches.
///
/// Every event is inserted into `conversation_events` first and only then handed
/// to the live subscribers, so the journal order *is* the delivery order, and a
/// reader that was not connected can be brought up to date from the rows.
pub struct EventLog {
    pool: SqlitePool,
    /// Live tails. Dropped when the last subscriber leaves, so idle conversations
    /// do not accumulate map entries for the lifetime of the process.
    streams: Mutex<HashMap<i64, broadcast::Sender<PublishedEvent>>>,
}

impl EventLog {
    pub fn new(pool: SqlitePool) -> Self {
        Self {
            pool,
            streams: Mutex::new(HashMap::new()),
        }
    }

    /// Persists the event and hands it to whoever is watching right now.
    /// Synchronous fan-out: the caller returns only after the log and the live
    /// tail agree, which is what makes the journal order authoritative.
    pub async fn publish(
        &self,
        conversation_id: i64,
        event: ConversationEvent,
    ) -> Result<PublishedEvent, AppError> {
        // One transaction because `seq` is per conversation and must not be
        // computed by two writers at once.
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query(
            "SELECT COALESCE(MAX(seq), 0) + 1 AS next FROM conversation_events WHERE conversation_id = ?1",
        )
        .bind(conversation_id)
        .fetch_one(&mut *tx)
        .await?;
        let seq: i64 = row.try_get("next")?;

        let payload = serde_json::to_string(&event).map_err(|error| {
            AppError::internal(anyhow::anyhow!("encoding a journal payload: {error}"))
        })?;
        sqlx::query(
            "INSERT INTO conversation_events (conversation_id, seq, message_id, type, payload, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .bind(conversation_id)
        .bind(seq)
        .bind(event.message_id_of())
        .bind(event.type_name())
        .bind(&payload)
        .bind(chrono::Utc::now().timestamp_millis())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;

        let published = PublishedEvent {
            seq,
            event: event.clone(),
            payload: payload.clone(),
        };
        if let Some(sender) = self.streams.lock().expect("stream lock").get(&conversation_id) {
            // A full channel means a subscriber fell behind; that reader's cursor
            // is stale and it will be told to resync, so the send can be ignored.
            let _ = sender.send(published.clone());
        }
        Ok(published)
    }

    /// Live tail only; replay is the reader's job (see `replay_after`).
    pub fn subscribe(&self, conversation_id: i64) -> broadcast::Receiver<PublishedEvent> {
        let mut streams = self.streams.lock().expect("stream lock");
        let sender = streams.entry(conversation_id).or_insert_with(|| {
            let (sender, _) = broadcast::channel(4096);
            sender
        });
        sender.subscribe()
    }

    /// Called when a reader's stream ends.
    pub fn release(&self, conversation_id: i64) {
        let mut streams = self.streams.lock().expect("stream lock");
        if streams.get(&conversation_id).is_some_and(|sender| sender.receiver_count() == 0) {
            streams.remove(&conversation_id);
        }
    }

    /// Conversation seq of the most recent event, 0 when the conversation has none.
    pub async fn latest_seq(&self, conversation_id: i64) -> Result<i64, AppError> {
        self.scalar_seq("SELECT COALESCE(MAX(seq), 0) AS value FROM conversation_events WHERE conversation_id = ?1", conversation_id).await
    }

    /// Oldest retained seq, or None when nothing is retained (never published).
    pub async fn oldest_seq(&self, conversation_id: i64) -> Result<Option<i64>, AppError> {
        let row = sqlx::query(
            "SELECT MIN(seq) AS value FROM conversation_events WHERE conversation_id = ?1",
        )
        .bind(conversation_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.try_get::<Option<i64>, _>("value").expect("value"))
    }

    /// Events a cursor missed, oldest first. A cursor strictly below the retained
    /// window is a gap the caller must report as `resync`, not pad with a partial
    /// replay — this method itself does not detect that; see `has_gap`.
    pub async fn replay_after(
        &self,
        conversation_id: i64,
        seq: i64,
    ) -> Result<Vec<PublishedEvent>, AppError> {
        let rows = sqlx::query(
            "SELECT seq, payload FROM conversation_events WHERE conversation_id = ?1 AND seq > ?2 ORDER BY seq ASC",
        )
        .bind(conversation_id)
        .bind(seq)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .filter_map(|row| {
                // Server-written JSON of our own event union; a corrupt row is
                // dropped rather than guessed at, since a frame missing from a
                // replay is recoverable by the client while a wrong one is not.
                let payload: String = row.try_get("payload").ok()?;
                serde_json::from_str::<ConversationEvent>(&payload)
                    .ok()
                    .map(|event| PublishedEvent {
                        payload,
                        seq: row.try_get("seq").expect("seq"),
                        event,
                    })
            })
            .collect())
    }

    /// Whether a cursor cannot be honored: it precedes the retained window. Equal
    /// to the oldest id minus one, or newer, is replayable — the cursor itself
    /// need not exist, only everything after it.
    pub async fn has_gap(&self, conversation_id: i64, cursor: i64) -> Result<bool, AppError> {
        Ok(self
            .oldest_seq(conversation_id)
            .await?
            .is_some_and(|oldest| cursor < oldest - 1))
    }

    /// Catch-up starting point for a conversation with a still-unfinished
    /// assistant row: the event just before that turn's first journal entry, so a
    /// fresh connection replays the live turn from its beginning. Falls back to
    /// the newest seq when the turn predates the retained window (gap → resync).
    pub async fn seq_before_turn(
        &self,
        conversation_id: i64,
        message_id: i64,
    ) -> Result<i64, AppError> {
        let row = sqlx::query(
            "SELECT MIN(seq) AS value FROM conversation_events WHERE conversation_id = ?1 AND message_id = ?2",
        )
        .bind(conversation_id)
        .bind(message_id)
        .fetch_one(&self.pool)
        .await?;
        match row.try_get::<Option<i64>, _>("value").expect("value") {
            // A turn's user_message precedes turn_started by one row in the same
            // tick; starting one before the turn's first row also replays the echo.
            Some(first) => Ok(first - 1),
            None => self.latest_seq(conversation_id).await,
        }
    }

    /// Drops everything older than the retained window. Called when a turn ends,
    /// so a conversation's journal stays small while a live turn's events — the
    /// newest ones — are never pruned out from under a reconnecting client.
    pub async fn prune(&self, conversation_id: i64) -> Result<(), AppError> {
        let row = sqlx::query(
            "SELECT COUNT(*) AS total FROM conversation_events WHERE conversation_id = ?1",
        )
        .bind(conversation_id)
        .fetch_one(&self.pool)
        .await?;
        let total: i64 = row.try_get("total")?;
        if total <= RETAINED_EVENTS {
            return Ok(());
        }
        // Keep the newest RETAINED_EVENTS rows; delete everything at or below the
        // boundary. `seq` is dense per conversation, so no subquery limit is
        // needed (SQLite does not accept one in DELETE).
        let boundary = total - RETAINED_EVENTS;
        sqlx::query("DELETE FROM conversation_events WHERE conversation_id = ?1 AND seq <= ?2")
            .bind(conversation_id)
            .bind(boundary)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn scalar_seq(&self, sql: &str, conversation_id: i64) -> Result<i64, AppError> {
        let row = sqlx::query(sql)
            .bind(conversation_id)
            .fetch_one(&self.pool)
            .await?;
        Ok(row.try_get::<i64, _>("value")?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::{TaskKind, TaskStatus};

    async fn log() -> EventLog {
        EventLog::new(setup_conversation().await)
    }

    /// A conversation row is needed: the journal has a foreign key to it.
    async fn setup_conversation() -> SqlitePool {
        let pool = crate::db::open_in_memory().await;
        sqlx::query("INSERT INTO users (id, username, password_hash, role, created_at) VALUES (1,'u','h','user',1)")
            .execute(&pool)
            .await
            .expect("user");
        sqlx::query("INSERT INTO conversations (id, user_id, title, created_at, updated_at) VALUES (1,1,'t',1,1)")
            .execute(&pool)
            .await
            .expect("conversation");
        pool
    }

    fn text(seq_free: i64, delta: &str) -> ConversationEvent {
        ConversationEvent::Text { message_id: seq_free, delta: delta.to_string() }
    }

    #[tokio::test]
    async fn seq_is_dense_per_conversation_and_starts_at_one() {
        let log = log().await;
        for expected in 1..=3i64 {
            let published = log.publish(1, text(9, "x")).await.expect("publish");
            assert_eq!(published.seq, expected);
        }
        assert_eq!(log.latest_seq(1).await.expect("latest"), 3);
        assert_eq!(log.latest_seq(2).await.expect("empty"), 0);
    }

    #[tokio::test]
    async fn a_replay_returns_exactly_what_the_cursor_missed() {
        let log = log().await;
        for delta in ["a", "b", "c"] {
            log.publish(1, text(9, delta)).await.expect("publish");
        }
        let replayed = log.replay_after(1, 1).await.expect("replay");
        assert_eq!(replayed.len(), 2, "oldest first, after the cursor");
        assert_eq!(replayed[0].seq, 2);
        match &replayed[0].event {
            ConversationEvent::Text { delta, message_id } => {
                assert_eq!(delta, "b");
                assert_eq!(*message_id, 9);
            }
            other => panic!("unexpected replay: {other:?}"),
        }
        assert!(log.replay_after(1, 3).await.expect("tail").is_empty());
    }

    #[tokio::test]
    async fn gap_detection_uses_the_retained_window_not_the_whole_history() {
        let log = log().await;
        assert!(!log.has_gap(1, 0).await.expect("never published"), "empty journal is not a gap");
        for _ in 0..3 {
            log.publish(1, text(9, "x")).await.expect("publish");
        }
        assert_eq!(log.oldest_seq(1).await.expect("oldest"), Some(1));
        assert!(!log.has_gap(1, 0).await.expect("from the start"));
        assert!(log.has_gap(1, -1).await.expect("before the start"));
    }

    #[tokio::test]
    async fn catch_up_starts_one_row_before_the_running_turn() {
        let log = log().await;
        let user_message = ConversationEvent::UserMessage {
            message: sample_message(1, "user", "画一只红隼"),
        };
        log.publish(1, user_message).await.expect("user echo");
        log.publish(1, ConversationEvent::TurnStarted { message_id: 2, mode: None })
            .await
            .expect("turn start");
        log.publish(1, text(2, "…")).await.expect("delta");

        // The user echo is at 1, the turn at 2..; catching up on message 2 starts
        // at 1 so the echo is replayed too.
        assert_eq!(log.seq_before_turn(1, 2).await.expect("cursor"), 1);
        // A turn with no journal rows at all falls back to the newest seq.
        assert_eq!(log.seq_before_turn(1, 99).await.expect("fallback"), 3);
    }

    #[tokio::test]
    async fn pruning_keeps_the_newest_window() {
        let log = log().await;
        for _ in 0..(RETAINED_EVENTS + 50) {
            log.publish(1, text(9, "x")).await.expect("publish");
        }
        log.prune(1).await.expect("prune");
        assert_eq!(log.oldest_seq(1).await.expect("oldest"), Some(51));
        assert_eq!(log.latest_seq(1).await.expect("latest"), RETAINED_EVENTS + 50);
    }

    #[tokio::test]
    async fn a_subscriber_sees_events_published_after_it_connected() {
        let log = log().await;
        let mut receiver = log.subscribe(1);
        log.publish(1, text(9, "live")).await.expect("publish");
        let got = receiver.recv().await.expect("receive");
        assert_eq!(got.seq, 1);

        // While a reader is alive the tail stays, even if it has drained.
        log.publish(1, text(9, "still delivered")).await.expect("publish");
        assert_eq!(receiver.try_recv().expect("second event").seq, 2);
        assert!(
            log.streams.lock().expect("stream lock").contains_key(&1),
            "one live receiver keeps the subject"
        );

        // The tail is dropped once the last reader leaves, so an idle
        // conversation keeps nothing behind — and the journal is unaffected.
        drop(receiver);
        log.release(1);
        assert!(log.streams.lock().expect("stream lock").is_empty());
        assert_eq!(log.publish(1, text(9, "after")).await.expect("publish").seq, 3);
    }

    fn sample_message(id: i64, role: &str, content: &str) -> MessageView {
        MessageView {
            id,
            role: role.to_string(),
            content: content.to_string(),
            reasoning: None,
            tool_calls: None,
            mode: None,
            created_at: 1,
            assets: vec![],
        }
    }

    #[test]
    fn the_journal_payload_is_the_frame_body_verbatim() {
        let event = ConversationEvent::Asset {
            message_id: 4,
            id: 7,
            kind: TaskKind::Image,
            url: "/api/v1/assets/7/download".to_string(),
            mime: "image/png".to_string(),
            bytes: 123,
        };
        let encoded = serde_json::to_string(&event).expect("encode");
        assert!(encoded.contains("\"type\":\"asset\""));
        assert!(encoded.contains("\"messageId\":4"));
        assert!(encoded.contains("\"kind\":\"image\""));
        let decoded: ConversationEvent = serde_json::from_str(&encoded).expect("decode");
        assert_eq!(decoded, event);
        assert_eq!(event.message_id_of(), Some(4));
        assert_eq!(
            ConversationEvent::TaskUpdated {
                task_id: 1,
                kind: TaskKind::Video,
                status: TaskStatus::Running,
                error: None,
            }
            .message_id_of(),
            None
        );
    }
}
