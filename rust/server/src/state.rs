use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::path::{Path, PathBuf};

use sqlx::SqlitePool;

use crate::config::AppConfig;
use crate::conversation::{ConversationService, EventLog};
use crate::error::AppError;
use crate::media::service::MediaService;
use crate::task::service::TaskService;

/// Everything a handler needs. `AppState` is `Clone` (pool and client are
/// reference counted) and handed to axum by `State<_>`.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<AppConfig>,
    pub pool: SqlitePool,
    pub http: reqwest::Client,
    pub conversations: Arc<ConversationService>,
    pub events: Arc<EventLog>,
    pub media: Arc<MediaService>,
    /// One-time tickets for the voice socket's handshake.
    pub tickets: Arc<crate::voice::ticket::TicketStore>,
    pub tasks: Arc<TaskService>,
    /// Conversations with a turn running in this process. One turn at a time per
    /// conversation, so a double submit cannot bill the provider twice.
    pub running_turns: Arc<Mutex<HashSet<i64>>>,
    /// The provider key, resolved once at boot (env, then cc-switch, then the
    /// codex config). `None` means every generation path must fail loudly.
    pub api_key: Arc<Option<String>>,
}

impl AppState {
    pub fn new(config: AppConfig, pool: SqlitePool, api_key: Option<String>) -> Self {
        let config = Arc::new(config);
        let http = reqwest::Client::builder()
            .user_agent("kestrel-studio/0.1")
            .build()
            .expect("reqwest client");
        let events = Arc::new(EventLog::new(pool.clone()));
        let conversations = Arc::new(ConversationService::new(pool.clone(), Arc::clone(&config)));
        let media = Arc::new(MediaService::new(http.clone(), Arc::clone(&config)));
        let tasks = Arc::new(TaskService::new(pool.clone(), Arc::clone(&events)));
        Self {
            running_turns: Arc::new(Mutex::new(HashSet::new())),
            conversations,
            events,
            tickets: Arc::new(crate::voice::ticket::TicketStore::new()),
            media,
            tasks,
            config,
            api_key: Arc::new(api_key),
            pool,
            http,
        }
    }

    /// The provider key every generation call needs.
    ///
    /// Re-resolved on demand when boot found nothing: a key written into the
    /// environment afterwards, or a fresh cc-switch row, should not need a
    /// restart. Absent still, the answer is 503 with the same sentence the Nest
    /// server threw at boot.
    pub async fn require_key(&self) -> Result<String, AppError> {
        if let Some(key) = self.api_key.as_ref().clone() {
            return Ok(key);
        }
        if let Some(key) = crate::bailian::token::resolve_key().await {
            return Ok(key);
        }
        Err(AppError::unavailable(
            crate::bailian::token::MISSING_KEY_MESSAGE,
        ))
    }

    /// Whether a turn may start on this conversation right now. Takes the slot in
    /// the same step so two concurrent submits cannot both pass the check.
    pub fn claim_turn(&self, conversation_id: i64) -> bool {
        self.running_turns
            .lock()
            .expect("turn registry")
            .insert(conversation_id)
    }

    pub fn release_turn(&self, conversation_id: i64) {
        if let Ok(mut running) = self.running_turns.lock() {
            running.remove(&conversation_id);
        }
    }

    pub fn is_turn_running(&self, conversation_id: i64) -> bool {
        self.running_turns
            .lock()
            .map(|running| running.contains(&conversation_id))
            .unwrap_or(false)
    }

    /// Where the Vite output lives. The Nest build resolved it relative to
    /// `dist/`; the Rust binary runs from the project root, and `STATIC_DIR`
    /// overrides it for the image.
    pub fn public_dir(&self) -> PathBuf {
        std::env::var("STATIC_DIR")
            .ok()
            .map(PathBuf::from)
            .unwrap_or_else(|| Path::new("public").to_path_buf())
    }
}
