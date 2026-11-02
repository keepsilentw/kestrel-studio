use hmac::{Hmac, Mac};
use sha2::Sha256;
use sqlx::Row;
use sqlx::SqlitePool;

use crate::config::random_bytes;

/// Cookie name kept identical to express-session's default so an existing
/// browser cookie is at least recognised (the signature scheme differs, so it
/// will be rejected rather than trusted).
pub const COOKIE_NAME: &str = "connect.sid";
pub const MAX_AGE_MS: i64 = 7 * 24 * 60 * 60 * 1000;

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn sign(token: &str, secret: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC key length");
    mac.update(format!("s:{token}").as_bytes());
    let digest = mac.finalize().into_bytes();
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

/// `s:<token>.<signature>` — the shape a browser already expects from this app.
pub fn cookie_value(token: &str, secret: &str) -> String {
    format!("s:{token}.{}", sign(token, secret))
}

/// Builds the `Set-Cookie` header. Same attributes express-session used:
/// HttpOnly, SameSite=Lax, seven days, no `Secure` flag (the TLS termination is
/// the reverse proxy's, so the app never sees https).
pub fn set_cookie(token: &str, secret: &str) -> String {
    format!(
        "{COOKIE_NAME}={}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}",
        cookie_value(token, secret),
        MAX_AGE_MS / 1000
    )
}

pub fn cleared_cookie() -> String {
    format!("{COOKIE_NAME}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0")
}

/// Reads the token out of a `Cookie` header and checks its signature.
pub fn token_from_cookie(header: &str, secret: &str) -> Option<String> {
    for pair in header.split(';') {
        let pair = pair.trim();
        let Some(raw) = pair.strip_prefix(&format!("{COOKIE_NAME}=")) else {
            continue;
        };
        let body = raw.strip_prefix("s:")?;
        let (token, signature) = body.split_once('.')?;
        if token.is_empty() || !constant_time_eq(sign(token, secret).as_bytes(), signature.as_bytes())
        {
            return None;
        }
        return Some(token.to_string());
    }
    None
}

/// Inserts a row and returns its token. The JSON shape carries the principal id
/// under `user`, the same key passport used, so a row written here is readable
/// by the Nest server during a cutover.
pub async fn create(pool: &SqlitePool, user_id: i64) -> Result<String, crate::error::AppError> {
    let token = crate::config::hex::encode(&random_bytes(24));
    let sess = serde_json::json!({
        "user": user_id,
        "cookie": { "originalMaxAge": MAX_AGE_MS, "expires": now_ms() + MAX_AGE_MS },
    });
    sqlx::query("INSERT INTO sessions (sid, sess, expire) VALUES (?1, ?2, ?3)")
        .bind(&token)
        .bind(sess.to_string())
        .bind(now_ms() + MAX_AGE_MS)
        .execute(pool)
        .await?;
    Ok(token)
}

/// The principal id behind a token, or `None` when it is missing or expired.
/// Expired rows are deleted here rather than by a timer: one less background
/// task, and a stale row is only ever read at request time anyway.
pub async fn principal_id(pool: &SqlitePool, token: &str) -> Result<Option<i64>, crate::error::AppError> {
    let row = sqlx::query("SELECT sess, expire FROM sessions WHERE sid = ?1")
        .bind(token)
        .fetch_optional(pool)
        .await?;
    let Some(row) = row else { return Ok(None) };

    let expire: i64 = row.try_get("expire")?;
    if expire < now_ms() {
        sqlx::query("DELETE FROM sessions WHERE sid = ?1")
            .bind(token)
            .execute(pool)
            .await
            .ok();
        return Ok(None);
    }

    let sess: String = row.try_get("sess")?;
    let parsed: serde_json::Value = serde_json::from_str(&sess).unwrap_or(serde_json::Value::Null);
    let user_id = parsed
        .get("user")
        .and_then(serde_json::Value::as_i64)
        .or_else(|| parsed.get("passport").and_then(|value| value.get("user")).and_then(serde_json::Value::as_i64));
    Ok(user_id)
}

pub async fn destroy(pool: &SqlitePool, token: &str) -> Result<(), crate::error::AppError> {
    sqlx::query("DELETE FROM sessions WHERE sid = ?1")
        .bind(token)
        .execute(pool)
        .await?;
    Ok(())
}

/// Deletes rows whose expiry passed. Called at boot and on an interval; the Nest
/// store did the same every 15 minutes.
pub async fn sweep(pool: &SqlitePool) -> Result<u64, crate::error::AppError> {
    let result = sqlx::query("DELETE FROM sessions WHERE expire < ?1")
        .bind(now_ms())
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (left, right) in a.iter().zip(b) {
        diff |= left ^ right;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "unit-test-secret";

    #[test]
    fn a_signed_cookie_round_trips_only_with_the_right_secret() {
        let value = cookie_value("abc123", SECRET);
        let header = format!("other=1; {COOKIE_NAME}={value}; theme=dark");
        assert_eq!(token_from_cookie(&header, SECRET).as_deref(), Some("abc123"));
        assert_eq!(token_from_cookie(&header, "other-secret"), None);

        let forged = format!("{COOKIE_NAME}=s:abc123.deadbeef");
        assert_eq!(token_from_cookie(&forged, SECRET), None);
        assert_eq!(token_from_cookie(&format!("{COOKIE_NAME}=abc123"), SECRET), None);
        assert_eq!(token_from_cookie("", SECRET), None);
    }

    #[tokio::test]
    async fn a_session_stores_and_drops_its_principal() {
        let pool = crate::db::open_in_memory().await;
        sqlx::query("INSERT INTO users (username, password_hash, role, created_at) VALUES ('u','h','user',1)")
            .execute(&pool)
            .await
            .expect("seed user");

        let token = create(&pool, 1).await.expect("create");
        assert_eq!(principal_id(&pool, &token).await.expect("read"), Some(1));
        assert_eq!(principal_id(&pool, "nosuchtoken").await.expect("read"), None);

        destroy(&pool, &token).await.expect("destroy");
        assert_eq!(principal_id(&pool, &token).await.expect("read"), None);
    }

    #[tokio::test]
    async fn an_expired_row_reads_as_anonymous_and_is_dropped_on_read() {
        let pool = crate::db::open_in_memory().await;
        sqlx::query("INSERT INTO sessions (sid, sess, expire) VALUES ('old', '{\"user\":7}', 1)")
            .execute(&pool)
            .await
            .expect("insert");
        assert_eq!(principal_id(&pool, "old").await.expect("read"), None);
        // Reading it already deleted the row: a stale row is only ever touched at
        // request time, so nothing is left for the boot sweep to clear.
        assert_eq!(sweep(&pool).await.expect("sweep"), 0);

        sqlx::query("INSERT INTO sessions (sid, sess, expire) VALUES ('stale', '{\"user\":7}', 1)")
            .execute(&pool)
            .await
            .expect("insert unread row");
        assert_eq!(sweep(&pool).await.expect("sweep"), 1);
        assert_eq!(principal_id(&pool, "stale").await.expect("read"), None);
    }
}
