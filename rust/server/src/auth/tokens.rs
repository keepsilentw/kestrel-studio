use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::Row;
use sqlx::SqlitePool;

use crate::config::random_bytes;
use crate::error::AppError;

/// Every device token carries this prefix, so it cannot be confused with a
/// session sid (which is bare hex) when the same Bearer header is resolved.
pub const TOKEN_PREFIX: &str = "kst_";

/// `last_seen_at` is refreshed at most once an hour. Without the bound, every
/// read-only request would issue an UPDATE and turn SQLite's single writer into
/// the bottleneck for the whole product API (docs/roadmap.md §5.1).
const SEEN_THROTTLE_MS: i64 = 60 * 60 * 1000;

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// The stored form is the SHA-256 digest, hex-encoded. The plaintext exists only
/// in the response that issued it.
pub fn hash(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    crate::config::hex::encode(&digest)
}

/// A blank label falls back to a human-readable default; a long one is capped
/// rather than rejected, because the label is a hint and not an identifier.
pub fn normalize_label(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "未命名设备".to_string();
    }
    trimmed.chars().take(64).collect()
}

pub struct IssuedToken {
    pub id: i64,
    pub token: String,
    pub created_at: i64,
}

pub async fn issue(
    pool: &SqlitePool,
    user_id: i64,
    label: &str,
) -> Result<IssuedToken, AppError> {
    let token = format!(
        "{TOKEN_PREFIX}{}",
        crate::config::hex::encode(&random_bytes(24))
    );
    let created_at = now_ms();
    let result = sqlx::query(
        "INSERT INTO api_tokens (user_id, label, token_hash, created_at) VALUES (?1, ?2, ?3, ?4)",
    )
    .bind(user_id)
    .bind(normalize_label(label))
    .bind(hash(&token))
    .bind(created_at)
    .execute(pool)
    .await?;
    Ok(IssuedToken {
        id: result.last_insert_rowid(),
        token,
        created_at,
    })
}

/// The user behind an active device token, refreshing `last_seen_at` at most once
/// per hour. Revoked rows resolve to `None` exactly like an unknown token.
pub async fn resolve(pool: &SqlitePool, token: &str) -> Result<Option<i64>, AppError> {
    let row = sqlx::query(
        "SELECT id, user_id, last_seen_at FROM api_tokens \
         WHERE token_hash = ?1 AND revoked_at IS NULL",
    )
    .bind(hash(token))
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else { return Ok(None) };

    let id: i64 = row.try_get("id")?;
    let user_id: i64 = row.try_get("user_id")?;
    let last_seen: Option<i64> = row.try_get("last_seen_at")?;
    let now = now_ms();
    if last_seen.is_none_or(|seen| now - seen >= SEEN_THROTTLE_MS) {
        sqlx::query("UPDATE api_tokens SET last_seen_at = ?1 WHERE id = ?2 AND revoked_at IS NULL")
            .bind(now)
            .bind(id)
            .execute(pool)
            .await?;
    }
    Ok(Some(user_id))
}

/// Active tokens of one account, newest first. Revoked rows stay in the table as
/// an audit trail but are never listed.
pub async fn list(pool: &SqlitePool, user_id: i64) -> Result<Vec<Value>, AppError> {
    let rows = sqlx::query(
        "SELECT id, label, created_at, last_seen_at FROM api_tokens \
         WHERE user_id = ?1 AND revoked_at IS NULL ORDER BY id DESC",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            json!({
                "id": row.try_get::<i64, _>("id").unwrap_or_default(),
                "label": row.try_get::<String, _>("label").unwrap_or_default(),
                "createdAt": row.try_get::<i64, _>("created_at").unwrap_or_default(),
                "lastSeenAt": row.try_get::<Option<i64>, _>("last_seen_at").unwrap_or(None),
            })
        })
        .collect())
}

/// Revocation is scoped to the owner: another account's id reads as `false`,
/// which the handler turns into 404 rather than confirming the row exists.
pub async fn revoke(pool: &SqlitePool, user_id: i64, id: i64) -> Result<bool, AppError> {
    let result = sqlx::query(
        "UPDATE api_tokens SET revoked_at = ?1 \
         WHERE id = ?2 AND user_id = ?3 AND revoked_at IS NULL",
    )
    .bind(now_ms())
    .bind(id)
    .bind(user_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_issued_token_resolves_and_never_lands_in_plaintext() {
        let pool = crate::db::open_in_memory().await;
        sqlx::query("INSERT INTO users (username, password_hash, role, created_at) VALUES ('u','h','user',1)")
            .execute(&pool)
            .await
            .expect("seed user");

        let issued = issue(&pool, 1, "  phone  ").await.expect("issue");
        assert!(issued.token.starts_with(TOKEN_PREFIX));
        assert_eq!(resolve(&pool, &issued.token).await.expect("resolve"), Some(1));
        assert_eq!(resolve(&pool, "kst_unknown").await.expect("resolve"), None);

        let stored: String = sqlx::query("SELECT token_hash FROM api_tokens WHERE id = ?1")
            .bind(issued.id)
            .fetch_one(&pool)
            .await
            .expect("stored row")
            .try_get("token_hash")
            .expect("hash");
        assert_eq!(stored, hash(&issued.token));
        assert_ne!(stored, issued.token);

        let listed = list(&pool, 1).await.expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0]["label"], "phone");
        assert!(listed[0]["lastSeenAt"].as_i64().is_some());
    }

    #[tokio::test]
    async fn a_revoked_token_stops_resolving_and_drops_out_of_the_list() {
        let pool = crate::db::open_in_memory().await;
        sqlx::query("INSERT INTO users (username, password_hash, role, created_at) VALUES ('u','h','user',1)")
            .execute(&pool)
            .await
            .expect("seed user");
        let issued = issue(&pool, 1, "").await.expect("issue");

        assert!(revoke(&pool, 1, issued.id).await.expect("revoke"));
        assert!(!revoke(&pool, 1, issued.id).await.expect("second revoke"));
        assert!(!revoke(&pool, 2, issued.id).await.expect("other owner"));
        assert_eq!(resolve(&pool, &issued.token).await.expect("resolve"), None);
        assert!(list(&pool, 1).await.expect("list").is_empty());
    }

    #[tokio::test]
    async fn last_seen_is_not_rewritten_inside_the_hour() {
        let pool = crate::db::open_in_memory().await;
        sqlx::query("INSERT INTO users (username, password_hash, role, created_at) VALUES ('u','h','user',1)")
            .execute(&pool)
            .await
            .expect("seed user");
        let issued = issue(&pool, 1, "laptop").await.expect("issue");
        resolve(&pool, &issued.token).await.expect("first");
        let first: Option<i64> = sqlx::query("SELECT last_seen_at FROM api_tokens WHERE id = ?1")
            .bind(issued.id)
            .fetch_one(&pool)
            .await
            .expect("row")
            .try_get("last_seen_at")
            .expect("seen");

        resolve(&pool, &issued.token).await.expect("second");
        let second: Option<i64> = sqlx::query("SELECT last_seen_at FROM api_tokens WHERE id = ?1")
            .bind(issued.id)
            .fetch_one(&pool)
            .await
            .expect("row")
            .try_get("last_seen_at")
            .expect("seen");
        assert_eq!(first, second, "no second write inside the throttle window");
    }
}
