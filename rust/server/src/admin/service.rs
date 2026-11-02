use serde::Serialize;
use sqlx::{Row, SqlitePool};
use std::sync::Arc;

use crate::auth::password;
use crate::auth::roles::{parse_role, Principal, Role};
use crate::conversation::ConversationService;
use crate::error::AppError;

/// Accounts are typed by hand on the admin page, so keep them URL-safe.
const USERNAME_MIN: usize = 3;
const USERNAME_MAX: usize = 32;
const MIN_PASSWORD_LENGTH: usize = 6;
const MAX_PASSWORD_LENGTH: usize = 128;

fn is_acceptable_username(name: &str) -> bool {
    let length = name.chars().count();
    (USERNAME_MIN..=USERNAME_MAX).contains(&length)
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

pub fn is_acceptable_password(password: &str) -> bool {
    password.trim().chars().count() >= MIN_PASSWORD_LENGTH && password.chars().count() <= MAX_PASSWORD_LENGTH
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountView {
    pub id: i64,
    pub username: String,
    #[serde(skip)]
    pub role: Role,
    pub role_label: &'static str,
    pub created_at: i64,
    pub conversation_count: i64,
    pub asset_count: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateAccountResult {
    Created(i64),
    InvalidUsername,
    InvalidPassword,
    Duplicate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetPasswordResult {
    Reset,
    InvalidPassword,
    Missing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoveAccountResult {
    Removed { conversations: usize, assets: usize },
    Missing,
    SelfAccount,
    LastSuperAdmin,
}

pub struct AdminService {
    pool: SqlitePool,
    conversations: Arc<ConversationService>,
}

impl AdminService {
    pub fn new(pool: SqlitePool, conversations: Arc<ConversationService>) -> Self {
        Self { pool, conversations }
    }

    pub async fn list(&self) -> Result<Vec<AccountView>, AppError> {
        let conversation_counts = self.grouped_conversation_counts().await?;
        let asset_counts = self.grouped_asset_counts().await?;
        let rows = sqlx::query("SELECT id, username, role, created_at FROM users ORDER BY id ASC")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .iter()
            .map(|row| {
                let id: i64 = row.try_get("id").expect("id");
                to_account(
                    row,
                    id,
                    conversation_counts.get(&id).copied().unwrap_or(0),
                    asset_counts.get(&id).copied().unwrap_or(0),
                )
            })
            .collect())
    }

    pub async fn find(&self, id: i64) -> Result<Option<AccountView>, AppError> {
        let row = sqlx::query("SELECT id, username, role, created_at FROM users WHERE id = ?1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        let Some(row) = row else { return Ok(None) };
        let mut counts = self.grouped_conversation_counts().await?;
        let mut assets = self.grouped_asset_counts().await?;
        Ok(Some(to_account(
            &row,
            id,
            counts.remove(&id).unwrap_or(0),
            assets.remove(&id).unwrap_or(0),
        )))
    }

    /// New accounts are always plain users; promotion is not part of this surface.
    pub async fn create(&self, username: &str, password_plain: &str) -> Result<CreateAccountResult, AppError> {
        let name = username.trim();
        if !is_acceptable_username(name) {
            return Ok(CreateAccountResult::InvalidUsername);
        }
        if !is_acceptable_password(password_plain) {
            return Ok(CreateAccountResult::InvalidPassword);
        }

        // Hash before checking, as the Nest version did, so the window between the
        // existence check and the insert stays as small as it can. The UNIQUE index
        // is what actually closes it: a collision becomes `Duplicate` rather than a
        // second row.
        let hash = password::hash(password_plain);
        let existing: Option<(i64,)> = sqlx::query_as("SELECT id FROM users WHERE username = ?1")
            .bind(name)
            .fetch_optional(&self.pool)
            .await?;
        if existing.is_some() {
            return Ok(CreateAccountResult::Duplicate);
        }

        let inserted = sqlx::query("INSERT INTO users (username, password_hash, role, created_at) VALUES (?1, ?2, 'user', ?3)")
            .bind(name)
            .bind(&hash)
            .bind(chrono::Utc::now().timestamp_millis())
            .execute(&self.pool)
            .await;
        match inserted {
            Ok(result) => Ok(CreateAccountResult::Created(result.last_insert_rowid())),
            Err(error) if is_unique_violation(&error) => Ok(CreateAccountResult::Duplicate),
            Err(error) => Err(AppError::from(error)),
        }
    }

    pub async fn reset_password(&self, id: i64, password_plain: &str) -> Result<ResetPasswordResult, AppError> {
        if !is_acceptable_password(password_plain) {
            return Ok(ResetPasswordResult::InvalidPassword);
        }
        let hash = password::hash(password_plain);
        let result = sqlx::query("UPDATE users SET password_hash = ?1 WHERE id = ?2")
            .bind(&hash)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(if result.rows_affected() > 0 {
            ResetPasswordResult::Reset
        } else {
            ResetPasswordResult::Missing
        })
    }

    /// Removes an account and everything it owns: conversations, messages, tasks,
    /// asset rows and the mirrored files.
    ///
    /// The conversation cascade is `ConversationService`'s (shared with deleting a
    /// single conversation); the account row goes after it, so a failure there
    /// leaves an account with no conversations rather than orphaned rows.
    ///
    /// Refuses to remove the acting account (which would end the session mid-flight)
    /// and the last super admin (which would leave nobody able to manage accounts).
    pub async fn remove(&self, id: i64, actor: &Principal) -> Result<RemoveAccountResult, AppError> {
        if id == actor.id {
            return Ok(RemoveAccountResult::SelfAccount);
        }
        let target = sqlx::query("SELECT role FROM users WHERE id = ?1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        let Some(target) = target else {
            return Ok(RemoveAccountResult::Missing);
        };
        if parse_role(target.try_get("role").ok()) == Role::Super && self.super_admin_count().await? <= 1 {
            return Ok(RemoveAccountResult::LastSuperAdmin);
        }

        let rows = sqlx::query("SELECT id FROM conversations WHERE user_id = ?1")
            .bind(id)
            .fetch_all(&self.pool)
            .await?;
        let ids: Vec<i64> = rows.iter().filter_map(|row| row.try_get("id").ok()).collect();

        let (conversations, assets) = self.conversations.delete_conversations(&ids).await?;
        sqlx::query("DELETE FROM users WHERE id = ?1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(RemoveAccountResult::Removed { conversations, assets })
    }

    async fn grouped_conversation_counts(&self) -> Result<std::collections::HashMap<i64, i64>, AppError> {
        let rows = sqlx::query("SELECT user_id, COUNT(*) AS total FROM conversations GROUP BY user_id")
            .fetch_all(&self.pool)
            .await?;
        Ok(grouped(&rows))
    }

    async fn grouped_asset_counts(&self) -> Result<std::collections::HashMap<i64, i64>, AppError> {
        let rows = sqlx::query(
            "SELECT c.user_id AS user_id, COUNT(*) AS total FROM assets a JOIN messages m ON m.id = a.message_id JOIN conversations c ON c.id = m.conversation_id GROUP BY c.user_id",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(grouped(&rows))
    }

    async fn super_admin_count(&self) -> Result<i64, AppError> {
        let row = sqlx::query("SELECT COUNT(*) AS total FROM users WHERE role = 'super'")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.try_get("total").unwrap_or(0))
    }
}

fn grouped(rows: &[sqlx::sqlite::SqliteRow]) -> std::collections::HashMap<i64, i64> {
    rows.iter()
        .filter_map(|row| Some((row.try_get::<i64, _>("user_id").ok()?, row.try_get::<i64, _>("total").ok()?)))
        .collect()
}

fn to_account(row: &sqlx::sqlite::SqliteRow, id: i64, conversation_count: i64, asset_count: i64) -> AccountView {
    let role = parse_role(row.try_get("role").ok());
    AccountView {
        id,
        username: row.try_get("username").unwrap_or_default(),
        role,
        role_label: role.label(),
        created_at: row.try_get("created_at").unwrap_or(0),
        conversation_count,
        asset_count,
    }
}

fn is_unique_violation(error: &sqlx::Error) -> bool {
    match error {
        sqlx::Error::Database(database) => database
            .message()
            .contains("UNIQUE constraint failed: users.username"),
        _ => false,
    }
}
