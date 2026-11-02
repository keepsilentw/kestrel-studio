use std::path::Path;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};
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
CREATE INDEX IF NOT EXISTS idx_events_conversation ON conversation_events(conversation_id, seq);

CREATE TABLE IF NOT EXISTS sessions (
  sid TEXT NOT NULL PRIMARY KEY,
  sess TEXT NOT NULL,
  expire INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_sessions_expire ON sessions(expire);
"#;

/// `CREATE TABLE IF NOT EXISTS` cannot add a column to a table that already
/// exists, and this database file survives deploys. So columns introduced after
/// the first release are added explicitly here.
const ADDED_COLUMNS: [(&str, &str, &str); 3] = [
    ("messages", "mode", "TEXT"),
    // Every account that existed before roles did lands on the least privileged
    // role; the seeded super admin is created with `super` explicitly.
    ("users", "role", "TEXT NOT NULL DEFAULT 'user'"),
    // Null means live; a timestamp hides the conversation from its owner only.
    ("conversations", "deleted_at", "INTEGER"),
];

pub async fn open(config: &AppConfig) -> Result<SqlitePool, crate::error::AppError> {
    let path = config.database_file.as_str();
    if let Some(parent) = Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).ok();
        }
    }

    let options = SqliteConnectOptions::new()
        .filename(Path::new(path))
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal);

    let pool = SqlitePoolOptions::new()
        // SQLite takes one writer at a time; the pool is deliberately single
        // connection so `BEGIN IMMEDIATE` never deadlocks against itself.
        .max_connections(1)
        .connect_with(options)
        .await
        .map_err(|error| {
            crate::error::AppError::internal(anyhow::anyhow!("opening {path}: {error}"))
        })?;

    apply_schema(&pool).await?;
    Ok(pool)
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
        let exists = rows
            .iter()
            .any(|row| row.try_get::<String, _>("name").is_ok_and(|name| name == column));
        if !exists {
            sqlx::query(&format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"))
                .execute(pool)
                .await
                .map_err(|error| {
                    crate::error::AppError::internal(anyhow::anyhow!(
                        "adding {table}.{column}: {error}"
                    ))
                })?;
        }
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

        let inserted = sqlx::query(
            "INSERT INTO users (username, password_hash, role, created_at) VALUES ('u', 'h', 'user', 1)",
        )
        .execute(&pool)
        .await
        .expect("insert");
        assert!(inserted.last_insert_rowid() > 0);
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
