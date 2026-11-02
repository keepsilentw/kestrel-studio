use sqlx::SqlitePool;

use crate::config::SeedAccount;
use crate::error::AppError;

use super::password;
use super::roles::Role;

/// The plain account kept for first-run local access; no privileges beyond its
/// own data. It is the only credential the repository ships, and it is worth
/// changing before anything leaves the laptop.
pub const DEFAULT_ADMIN_USERNAME: &str = "admin";
pub const DEFAULT_ADMIN_PASSWORD: &str = "123456";

async fn ensure_user(
    pool: &SqlitePool,
    username: &str,
    plain: &str,
    role: Role,
) -> Result<bool, AppError> {
    let existing: Option<(i64,)> =
        sqlx::query_as("SELECT id FROM users WHERE username = ?1")
            .bind(username)
            .fetch_optional(pool)
            .await?;
    if existing.is_some() {
        return Ok(false);
    }

    sqlx::query("INSERT INTO users (username, password_hash, role, created_at) VALUES (?1, ?2, ?3, ?4)")
        .bind(username)
        .bind(password::hash(plain))
        .bind(role.as_str())
        .bind(chrono::Utc::now().timestamp_millis())
        .execute(pool)
        .await?;
    Ok(true)
}

/// Idempotent: creates an account only when it is missing, so a restart never
/// resets a password the user has changed.
pub async fn ensure_admin_user(pool: &SqlitePool, is_production: bool) -> Result<bool, AppError> {
    if is_production {
        return Ok(false);
    }
    ensure_user(pool, DEFAULT_ADMIN_USERNAME, DEFAULT_ADMIN_PASSWORD, Role::User).await
}

/// The super admin is not a constant: its credentials arrive from the
/// environment, so the repository alone never describes a working login for a
/// deployed instance. With either variable absent nothing is seeded and `/admin`
/// stays unreachable. Like the default account, the password is applied **only
/// when the row is created** — rotate it from the admin page, not by editing the
/// environment file.
pub async fn ensure_super_admin(
    pool: &SqlitePool,
    account: Option<&SeedAccount>,
) -> Result<bool, AppError> {
    let Some(account) = account else {
        return Ok(false);
    };
    ensure_user(pool, &account.username, &account.password, Role::Super).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::Row;

    #[tokio::test]
    async fn seeding_is_idempotent_and_never_resets_a_changed_password() {
        let pool = crate::db::open_in_memory().await;
        assert!(ensure_admin_user(&pool, false).await.expect("seed"));
        assert!(!ensure_admin_user(&pool, false).await.expect("second run"));

        sqlx::query("UPDATE users SET password_hash = 'changed' WHERE username = 'admin'")
            .execute(&pool)
            .await
            .expect("change password");
        ensure_admin_user(&pool, false).await.expect("third run");

        let row = sqlx::query("SELECT password_hash FROM users WHERE username = 'admin'")
            .fetch_one(&pool)
            .await
            .expect("read back");
        assert_eq!(row.try_get::<String, _>("password_hash").expect("hash"), "changed");
    }

    #[tokio::test]
    async fn production_skips_the_convenience_account_and_absent_env_skips_the_super_admin() {
        let pool = crate::db::open_in_memory().await;
        assert!(!ensure_admin_user(&pool, true).await.expect("prod"));
        assert!(!ensure_super_admin(&pool, None).await.expect("no env"));

        let count: i64 = sqlx::query("SELECT COUNT(*) AS n FROM users")
            .fetch_one(&pool)
            .await
            .expect("count")
            .try_get("n")
            .expect("count value");
        assert_eq!(count, 0);

        let account = SeedAccount {
            username: "owner".to_string(),
            password: "s3cret".to_string(),
        };
        assert!(ensure_super_admin(&pool, Some(&account)).await.expect("seed super"));
        assert_eq!(
            crate::auth::service::AuthService::new(pool.clone())
                .validate("owner", "s3cret")
                .await
                .expect("validate")
                .map(|user| user.role),
            Some(Role::Super)
        );
    }
}
