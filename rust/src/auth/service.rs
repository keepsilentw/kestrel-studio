use sqlx::Row;
use sqlx::SqlitePool;

use crate::error::AppError;

use super::roles::{parse_role, Principal, Role};

/// The single conversion from a `users` row to the principal carried in the
/// session, so the role cannot be forgotten on one of the paths that build it.
fn to_principal(id: i64, username: String, role: Option<&str>) -> Principal {
    Principal {
        id,
        username,
        role: parse_role(role),
    }
}

pub struct AuthService {
    pool: SqlitePool,
}

impl AuthService {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Checks credentials the same way the passport strategy did: an unknown
    /// username and a wrong password are indistinguishable to the caller.
    pub async fn validate(&self, username: &str, password: &str) -> Result<Option<Principal>, AppError> {
        let row = sqlx::query("SELECT id, username, password_hash, role FROM users WHERE username = ?1")
            .bind(username)
            .fetch_optional(&self.pool)
            .await?;
        let Some(row) = row else { return Ok(None) };

        let stored: String = row.try_get("password_hash")?;
        if super::password::verify(password, &stored) {
            Ok(Some(to_principal(
                row.try_get("id")?,
                row.try_get("username")?,
                row.try_get("role")?,
            )))
        } else {
            Ok(None)
        }
    }

    /// Used by the session layer to rebuild the principal from its id.
    pub async fn find_by_id(&self, id: i64) -> Result<Option<Principal>, AppError> {
        let row = sqlx::query("SELECT id, username, role FROM users WHERE id = ?1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|row| {
            to_principal(
                row.try_get("id").expect("id"),
                row.try_get("username").expect("username"),
                row.try_get("role").expect("role row has the column"),
            )
        }))
    }

    pub async fn role_of(&self, id: i64) -> Result<Option<Role>, AppError> {
        let row = sqlx::query("SELECT role FROM users WHERE id = ?1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|row| parse_role(row.try_get("role").ok())))
    }
}
