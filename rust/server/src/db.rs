use std::path::Path;

use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous,
};
use sqlx::Row;

use crate::config::AppConfig;

/// Initial schema, kept in sync with the Nest-era `database/schema.ts` by hand.
/// Schema changes that need data migration go here rather than to a migration
/// tool, because the database file survives deploys (it is a docker volume).
const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS users (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  username TEXT NOT NULL UNIQUE,
  password_hash TEXT NOT NULL,
  role TEXT NOT NULL DEFAULT 'user',
  created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS conversations (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  user_id INTEGER NOT NULL REFERENCES users(id),
  title TEXT NOT NULL,
  deleted_at INTEGER,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS messages (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  conversation_id INTEGER NOT NULL REFERENCES conversations(id),
  role TEXT NOT NULL,
  content TEXT NOT NULL,
  reasoning TEXT,
  tool_calls TEXT,
  -- Which backend wrote the row (`rust` / `nest`). The boot recovery of abandoned
  -- turns keys off it, so a restart of one stack cannot end the other's live turn.
  -- NULL means a row predating the column (see `abandon_stale_placeholders`).
  owner TEXT,
  created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS assets (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  message_id INTEGER NOT NULL REFERENCES messages(id),
  kind TEXT NOT NULL,
  file_path TEXT NOT NULL,
  source_url TEXT,
  mime TEXT NOT NULL,
  bytes INTEGER NOT NULL,
  -- Pixel dimensions of the stored bytes, read from the file's own header at write
  -- time. Nullable: a video, an unrecognised header, or a row predating this column
  -- all mean "the <img> gets no width/height", never "guess one" — the request's size
  -- is not it (measured 2026-11-09: asked 1024*448, provider returned 1536x640).
  width INTEGER,
  height INTEGER,
  created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS conversation_events (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  conversation_id INTEGER NOT NULL REFERENCES conversations(id),
  seq INTEGER NOT NULL,
  message_id INTEGER,
  type TEXT NOT NULL,
  payload TEXT NOT NULL,
  created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS generation_tasks (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  conversation_id INTEGER NOT NULL REFERENCES conversations(id),
  message_id INTEGER,
  kind TEXT NOT NULL,
  provider_task_id TEXT NOT NULL,
  model TEXT NOT NULL,
  prompt TEXT NOT NULL,
  params TEXT,
  status TEXT NOT NULL,
  attempts INTEGER NOT NULL DEFAULT 0,
  -- Lease: which backend is advancing this render right now, and since when. The
  -- worker claims a row with one conditional UPDATE before it touches the provider,
  -- so two stacks cannot poll, mirror and journal the same task twice.
  claimed_by TEXT,
  claimed_at INTEGER,
  error TEXT,
  asset_id INTEGER,
  result_message_id INTEGER,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  finished_at INTEGER
);

CREATE INDEX IF NOT EXISTS idx_conversations_user ON conversations(user_id, updated_at DESC);
CREATE INDEX IF NOT EXISTS idx_messages_conversation ON messages(conversation_id, id);
CREATE INDEX IF NOT EXISTS idx_assets_message ON assets(message_id);
CREATE INDEX IF NOT EXISTS idx_tasks_active ON generation_tasks(status, updated_at);
CREATE INDEX IF NOT EXISTS idx_tasks_conversation ON generation_tasks(conversation_id, id);
-- This one has to be UNIQUE, because `seq` is the stream cursor and two writers
-- computing the same value would otherwise both land, after which the `seq >`
-- filter hides the loser's row from live delivery and from replay alike. Plain
-- here on purpose: `enforce_unique_event_seq` is what makes it unique, and it can
-- repair the rows a non-unique era left behind before doing so. Declaring it
-- UNIQUE in this string would instead refuse to boot on such a database.
CREATE INDEX IF NOT EXISTS idx_events_conversation ON conversation_events(conversation_id, seq);

CREATE TABLE IF NOT EXISTS sessions (
  sid TEXT NOT NULL PRIMARY KEY,
  sess TEXT NOT NULL,
  -- TEXT to match the Nest session store, which writes ISO-8601 timestamps.
  expire TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_sessions_expire ON sessions(expire);

-- Revocable long-lived credentials for non-browser hosts (roadmap.md §5.1).
-- Only the SHA-256 of the token is stored: this file is a plain backup target,
-- and a leaked backup must not equal a working credential on every device.
CREATE TABLE IF NOT EXISTS api_tokens (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  user_id INTEGER NOT NULL REFERENCES users(id),
  label TEXT NOT NULL,
  token_hash TEXT NOT NULL UNIQUE,
  created_at INTEGER NOT NULL,
  -- Refreshed at most once an hour, so a read-only request does not turn into a
  -- write on every call (docs/roadmap.md §5.1).
  last_seen_at INTEGER,
  revoked_at INTEGER
);
CREATE INDEX IF NOT EXISTS idx_api_tokens_user ON api_tokens(user_id, revoked_at);
"#;

/// `CREATE TABLE IF NOT EXISTS` cannot add a column to a table that already
/// exists, and this database file survives deploys. So columns introduced after
/// the first release are added explicitly here.
const ADDED_COLUMNS: [(&str, &str, &str); 8] = [
    // The two dimensions the replay `<img>` reserves its box with. Read from the
    // stored bytes, not from the request: docs/verification.md §40.
    ("assets", "width", "INTEGER"),
    ("assets", "height", "INTEGER"),
    ("messages", "mode", "TEXT"),
    // Every account that existed before roles did lands on the least privileged
    // role; the seeded super admin is created with `super` explicitly.
    ("users", "role", "TEXT NOT NULL DEFAULT 'user'"),
    // Null means live; a timestamp hides the conversation from its owner only.
    ("conversations", "deleted_at", "INTEGER"),
    // The three columns that make the two backends safe to run side by side
    // (docs/architecture.md §13.2). All nullable: a row written before this
    // migration, or by a stack that has not run it yet, is simply unowned.
    ("messages", "owner", "TEXT"),
    ("generation_tasks", "claimed_by", "TEXT"),
    ("generation_tasks", "claimed_at", "INTEGER"),
];

/// The pragmas every file-backed connection runs with, in one place so the test
/// below checks what `open()` really applies instead of a copy of it.
fn connect_options(path: &Path) -> SqliteConnectOptions {
    SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        // WAL + NORMAL: a commit appends to the WAL without fsyncing it. Measured on
        // the deploy host 2026-11-20 (docs/verification.md §68): 1000 autocommit
        // inserts took 1519-1520 ms at FULL and 35 ms at NORMAL, and every journal
        // delta is its own commit. The price is the last few committed transactions
        // on a power cut, not corruption — and a snapshot backup runs nightly.
        .synchronous(SqliteSynchronous::Normal)
}

pub async fn open(config: &AppConfig) -> Result<SqlitePool, crate::error::AppError> {
    let path = config.database_file.as_str();
    if let Some(parent) = Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).ok();
        }
    }

    let pool = SqlitePoolOptions::new()
        // SQLite allows one writer at a time, but any number of readers under WAL.
        // With a single connection every page, auth lookup and asset query queued
        // behind the journal's per-delta commits. Writers still serialise on the
        // database write lock (5 s busy timeout) and `conversation/events.rs`
        // allocates `seq` inside the INSERT so two writers cannot pick the same one.
        .max_connections(config.db_pool_size)
        .connect_with(connect_options(Path::new(path)))
        .await
        .map_err(|error| {
            crate::error::AppError::internal(anyhow::anyhow!("opening {path}: {error}"))
        })?;

    apply_schema(&pool).await?;
    log_pool(&pool, config.db_pool_size).await;
    Ok(pool)
}

/// Reads the pragmas back instead of logging what was asked for — the point of the
/// line is that the running process really has them, which is also how a deploy is
/// checked (docs/verification.md §68).
async fn log_pool(pool: &SqlitePool, connections: u32) {
    let synchronous: i64 = sqlx::query_scalar("PRAGMA synchronous")
        .fetch_one(pool)
        .await
        .unwrap_or(-1);
    let journal: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(pool)
        .await
        .unwrap_or_default();
    tracing::info!(
        connections,
        synchronous,
        journal_mode = %journal,
        "sqlite pool ready"
    );
}

/// Brings a connection up to the current schema, pragmas included.
pub async fn apply_schema(pool: &SqlitePool) -> Result<(), crate::error::AppError> {
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(pool)
        .await
        .ok();

    for statement in DDL.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        sqlx::query(statement)
            .execute(pool)
            .await
            .map_err(|error| {
                crate::error::AppError::internal(anyhow::anyhow!(
                    "applying schema: {error}\n{statement}"
                ))
            })?;
    }

    for (table, column, definition) in ADDED_COLUMNS {
        // Table names come from ADDED_COLUMNS (compile-time constants), so they
        // are interpolated: pragma table-valued functions do not accept binds.
        let rows = sqlx::query(&format!("SELECT name FROM pragma_table_info('{table}')"))
            .fetch_all(pool)
            .await?;
        let exists = rows.iter().any(|row| {
            row.try_get::<String, _>("name")
                .is_ok_and(|name| name == column)
        });
        if !exists {
            sqlx::query(&format!(
                "ALTER TABLE {table} ADD COLUMN {column} {definition}"
            ))
            .execute(pool)
            .await
            .map_err(|error| {
                crate::error::AppError::internal(anyhow::anyhow!(
                    "adding {table}.{column}: {error}"
                ))
            })?;
        }
    }

    enforce_unique_event_seq(pool).await?;
    Ok(())
}

/// The journal's cursor index must be UNIQUE.
///
/// `seq` is per conversation and is what a reconnecting client resumes from, so a
/// duplicated pair is not cosmetic: live delivery and replay both read `seq >`, so
/// the row that lost the race is invisible to every reader forever. Until 2026-11-09
/// the value was allocated by a read-then-write that no transaction actually
/// serialized across processes, and the index was plain (docs/architecture.md §13.2).
///
/// Runs at every boot and is idempotent: a no-op once the index is unique, which is
/// the normal path. When it is not, the losers of each contested pair are moved to
/// the tail of that conversation's numbering in rowid order, keeping the lowest row
/// id at the contested value — that is the row cursors have already been served from.
/// Promoting an always-hidden row to the end of the journal is the least destructive
/// reading of an ordering that was never valid in the first place.
///
/// Loud but not fatal: a database whose journal cannot be made unique is a real bug,
/// yet refusing to boot trades a corrupt stream for no service at all.
async fn enforce_unique_event_seq(pool: &SqlitePool) -> Result<(), crate::error::AppError> {
    // `unique` is a reserved word, hence the quoting. The pragma returns one row per
    // index, so a missing name means the index has not been created yet.
    let row = sqlx::query(
        "SELECT \"unique\" AS is_unique FROM pragma_index_list('conversation_events') WHERE name = 'idx_events_conversation'",
    )
    .fetch_optional(pool)
    .await?;
    if row.is_some_and(|row| row.try_get::<i64, _>("is_unique").unwrap_or(0) == 1) {
        return Ok(());
    }

    let dups = sqlx::query(
        "SELECT e.conversation_id AS conversation_id, e.seq AS seq, e.id AS id \
         FROM conversation_events e \
         JOIN (SELECT conversation_id, seq FROM conversation_events \
               GROUP BY conversation_id, seq HAVING COUNT(*) > 1) d \
           ON d.conversation_id = e.conversation_id AND d.seq = e.seq \
         ORDER BY e.conversation_id ASC, e.seq ASC, e.id ASC",
    )
    .fetch_all(pool)
    .await?;

    // One pass, keeping the first rowid of every contested pair. `MAX(seq)` is
    // re-read per assignment so the tail keeps growing even across two groups of the
    // same conversation.
    let mut previous: Option<(i64, i64)> = None;
    let mut renumbered = 0;
    for dup in &dups {
        let conversation_id: i64 = dup.try_get("conversation_id")?;
        let seq: i64 = dup.try_get("seq")?;
        let id: i64 = dup.try_get("id")?;
        if previous == Some((conversation_id, seq)) {
            let next: i64 = sqlx::query(
                "SELECT COALESCE(MAX(seq), 0) + 1 AS next FROM conversation_events WHERE conversation_id = ?1",
            )
            .bind(conversation_id)
            .fetch_one(pool)
            .await?
            .try_get("next")?;
            sqlx::query("UPDATE conversation_events SET seq = ?1 WHERE id = ?2")
                .bind(next)
                .bind(id)
                .execute(pool)
                .await?;
            renumbered += 1;
        }
        previous = Some((conversation_id, seq));
    }

    // Drop-then-create rather than `CREATE UNIQUE INDEX IF NOT EXISTS`: the name is
    // already taken by the plain index the DDL above leaves in place.
    sqlx::query("DROP INDEX IF EXISTS idx_events_conversation")
        .execute(pool)
        .await?;
    match sqlx::query(
        "CREATE UNIQUE INDEX idx_events_conversation ON conversation_events(conversation_id, seq)",
    )
    .execute(pool)
    .await
    {
        // Worth an ERROR line even though the boot continues: rows that no reader
        // could ever have seen just became part of the conversation's history.
        Ok(_) if renumbered > 0 => {
            tracing::error!(
                renumbered,
                "repaired duplicate journal seq values and made the cursor index UNIQUE"
            );
        }
        Ok(_) => {}
        Err(error) => tracing::error!(
            %error,
            "the journal cursor index is still not UNIQUE — seq allocation is unsafe, see docs/architecture.md §13.2"
        ),
    }
    Ok(())
}
#[cfg(test)]
pub async fn open_in_memory() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .in_memory(true)
                .foreign_keys(true)
                .journal_mode(SqliteJournalMode::Memory),
        )
        .await
        .expect("in-memory sqlite");
    apply_schema(&pool).await.expect("schema");
    pool
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pragmas are the point of `connect_options`, and `:memory:` cannot show
    /// them (it runs `journal_mode=MEMORY`), so this opens a throwaway file. A
    /// regression to `synchronous=FULL` would show up as `2` here — and as the
    /// 1.5 s-per-1000-commits cost measured in docs/verification.md §68.
    #[tokio::test]
    async fn a_file_database_runs_wal_with_normal_synchronous() {
        let path =
            std::env::temp_dir().join(format!("kestrel-db-pragmas-{}.db", uuid::Uuid::new_v4()));
        let pool = SqlitePoolOptions::new()
            .max_connections(2)
            .connect_with(connect_options(&path))
            .await
            .expect("file sqlite");
        let synchronous: i64 = sqlx::query_scalar("PRAGMA synchronous")
            .fetch_one(&pool)
            .await
            .expect("synchronous");
        let journal: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(&pool)
            .await
            .expect("journal_mode");
        assert_eq!(synchronous, 1, "NORMAL is 1; FULL would be 2");
        assert_eq!(journal.to_lowercase(), "wal");
        pool.close().await;
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }

    #[tokio::test]
    async fn schema_is_idempotent_and_adds_later_columns() {
        let pool = open_in_memory().await;
        // Second pass must not fail, and must not re-add `mode`.
        apply_schema(&pool).await.expect("re-apply");

        let counts: Vec<(String,)> = sqlx::query_as(
            "SELECT name FROM pragma_table_info('messages') WHERE name IN ('mode','content')",
        )
        .fetch_all(&pool)
        .await
        .expect("table info");
        assert_eq!(counts.len(), 2);

        // The three columns that make two backends safe side by side are on the
        // table and survive a second pass (docs/architecture.md §13.2).
        let owned: Vec<(String,)> =
            sqlx::query_as("SELECT name FROM pragma_table_info('messages') WHERE name = 'owner'")
                .fetch_all(&pool)
                .await
                .expect("table info");
        assert_eq!(owned.len(), 1, "messages.owner exists exactly once");
        let leased: Vec<(String,)> = sqlx::query_as(
            "SELECT name FROM pragma_table_info('generation_tasks') WHERE name IN ('claimed_by','claimed_at')",
        )
        .fetch_all(&pool)
        .await
        .expect("table info");
        assert_eq!(leased.len(), 2, "the lease columns exist");

        let inserted = sqlx::query(
            "INSERT INTO users (username, password_hash, role, created_at) VALUES ('u', 'h', 'user', 1)",
        )
        .execute(&pool)
        .await
        .expect("insert");
        assert!(inserted.last_insert_rowid() > 0);
    }

    /// The cursor index has to be UNIQUE for `seq` to mean anything with two writers
    /// on one file. The DDL creates it plain on purpose, so this asserts the
    /// migration that follows actually converts it.
    #[tokio::test]
    async fn the_journal_cursor_index_is_unique() {
        let pool = open_in_memory().await;
        let row = sqlx::query(
            "SELECT \"unique\" AS is_unique FROM pragma_index_list('conversation_events') WHERE name = 'idx_events_conversation'",
        )
        .fetch_one(&pool)
        .await
        .expect("index list");
        assert_eq!(row.try_get::<i64, _>("is_unique").expect("flag"), 1);
    }

    /// A database created while the index was plain can already hold the damage:
    /// two rows sharing one `(conversation_id, seq)`. Re-applying the schema has to
    /// repair those rows, not fail the boot and not create the index over them.
    #[tokio::test]
    async fn duplicate_cursor_sequences_are_renumbered_before_the_index_is_unique() {
        let pool = open_in_memory().await;
        // Wind the database back to the pre-migration shape.
        sqlx::query("DROP INDEX idx_events_conversation")
            .execute(&pool)
            .await
            .expect("drop");
        sqlx::query(
            "CREATE INDEX idx_events_conversation ON conversation_events(conversation_id, seq)",
        )
        .execute(&pool)
        .await
        .expect("plain index");

        sqlx::query("INSERT INTO users (id, username, password_hash, role, created_at) VALUES (1,'u','h','user',1)")
            .execute(&pool)
            .await
            .expect("user");
        sqlx::query("INSERT INTO conversations (id, user_id, title, created_at, updated_at) VALUES (1,1,'t',1,1)")
            .execute(&pool)
            .await
            .expect("conversation");
        for (seq, type_name) in [(1, "a"), (1, "b"), (2, "c"), (2, "d")] {
            sqlx::query(
                "INSERT INTO conversation_events (conversation_id, seq, message_id, type, payload, created_at) VALUES (1, ?1, NULL, ?2, '{}', 1)",
            )
            .bind(seq)
            .bind(type_name)
            .execute(&pool)
            .await
            .expect("event");
        }

        apply_schema(&pool).await.expect("migrate");

        let kept: Vec<(i64, String)> =
            sqlx::query_as("SELECT seq, type FROM conversation_events ORDER BY seq ASC")
                .fetch_all(&pool)
                .await
                .expect("rows");
        // The lowest rowid of each contested pair keeps the value every cursor has
        // already been served from; the hidden loser moves to the tail in rowid order.
        let expected: Vec<(i64, String)> = [(1i64, "a"), (2, "c"), (3, "b"), (4, "d")]
            .into_iter()
            .map(|(seq, type_name)| (seq, type_name.to_string()))
            .collect();
        assert_eq!(
            kept, expected,
            "duplicates renumbered to the tail, keepers untouched"
        );
        let dups: i64 = sqlx::query(
            "SELECT COUNT(*) AS n FROM (SELECT conversation_id, seq FROM conversation_events GROUP BY conversation_id, seq HAVING COUNT(*) > 1)",
        )
        .fetch_one(&pool)
        .await
        .expect("count")
        .try_get("n")
        .expect("count");
        assert_eq!(dups, 0);
        let row = sqlx::query(
            "SELECT \"unique\" AS is_unique FROM pragma_index_list('conversation_events') WHERE name = 'idx_events_conversation'",
        )
        .fetch_one(&pool)
        .await
        .expect("index list");
        assert_eq!(row.try_get::<i64, _>("is_unique").expect("flag"), 1);
    }

    #[tokio::test]
    async fn foreign_keys_are_enforced() {
        let pool = open_in_memory().await;
        let error = sqlx::query(
            "INSERT INTO conversations (user_id, title, created_at, updated_at) VALUES (999, 't', 1, 1)",
        )
        .execute(&pool)
        .await
        .expect_err("orphan conversation");
        assert!(error.to_string().contains("FOREIGN KEY"));
    }
}
