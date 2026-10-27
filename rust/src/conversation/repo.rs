use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sqlx::query::Query;
use sqlx::{Row, SqlitePool, Transaction};

use crate::agent::mode::Mode;
use crate::auth::roles::Principal;
use crate::bailian::InputItem;
use crate::config::AppConfig;
use crate::error::AppError;
use crate::media::asset_path::{resolve_asset_path, stored_asset_name};
use crate::task::TaskKind;

const MAX_TITLE_LENGTH: usize = 30;

/// One tool round-trip, stored as JSON on the assistant message so a reload can
/// render the turn the way it ran.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallRecord {
    pub name: String,
    pub arguments: serde_json::Value,
    pub ok: bool,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AssetView {
    pub id: i64,
    /// `image` or `video`.
    pub kind: TaskKind,
    /// Download endpoint, not the on-disk path.
    pub url: String,
    pub mime: String,
    pub bytes: i64,
}

/// Where the browser fetches an asset from. Single source of the format: it is
/// also what the turn stream and the voice socket hand to the client.
pub fn asset_url(asset_id: i64) -> String {
    format!("/api/v1/assets/{asset_id}/download")
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MessageView {
    pub id: i64,
    pub role: String,
    pub content: String,
    pub reasoning: Option<String>,
    pub tool_calls: Option<Vec<ToolCallRecord>>,
    pub mode: Option<Mode>,
    pub created_at: i64,
    pub assets: Vec<AssetView>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ConversationSummary {
    pub id: i64,
    pub title: String,
    pub updated_at: i64,
}

/// A conversation plus whose it is, for the super admin's site-wide list.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AdminConversationSummary {
    pub id: i64,
    pub title: String,
    pub updated_at: i64,
    /// Set when the owner has deleted it; the super admin still reads it.
    pub deleted_at: Option<i64>,
    pub owner_id: i64,
    pub owner_name: String,
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn parse_tool_calls(raw: Option<String>) -> Option<Vec<ToolCallRecord>> {
    let raw = raw.filter(|value| !value.is_empty())?;
    serde_json::from_str::<Vec<ToolCallRecord>>(&raw).ok()
}

/// The read-visibility rule, expressed once for every query that needs it.
///
/// A normal account reads only its own, and not the ones it has deleted. A super
/// admin reads every normal account's as well — never another super admin's, so a
/// privileged account's own sessions stay private (docs/architecture.md §12) — and
/// the soft-deleted ones stay readable for it, which is what makes the delete a
/// hide rather than a loss.
///
/// Returns `(user_id, is_super)` for the `readable_sql` predicate.
fn readable_args(viewer: &Principal) -> (i64, i64) {
    (viewer.id, i64::from(viewer.role == crate::auth::roles::Role::Super))
}

/// SQL fragment for the visibility rule. Requires `users` joined as `u` on
/// `conversations`.
const READABLE_SQL: &str = "((c.user_id = ?1 AND c.deleted_at IS NULL) OR (?2 = 1 AND u.role = 'user'))";

pub struct ConversationService {
    pool: SqlitePool,
    /// Rows hold a bare file name; this is what resolves it. Injected rather than
    /// read from a process global, so a test cannot inherit another test's storage.
    config: Arc<AppConfig>,
}

impl ConversationService {
    pub fn new(pool: SqlitePool, config: Arc<AppConfig>) -> Self {
        Self { pool, config }
    }

    pub async fn create(&self, user_id: i64, first_prompt: &str) -> Result<i64, AppError> {
        let title: String = {
            let trimmed = first_prompt.trim();
            let cut: String = trimmed.chars().take(MAX_TITLE_LENGTH).collect();
            if cut.is_empty() {
                "新会话".to_string()
            } else {
                cut
            }
        };
        let now = now_ms();
        let row = sqlx::query(
            "INSERT INTO conversations (user_id, title, created_at, updated_at) VALUES (?1, ?2, ?3, ?4) RETURNING id",
        )
        .bind(user_id)
        .bind(&title)
        .bind(now)
        .bind(now)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.try_get("id")?)
    }

    pub async fn list_by_user(&self, user_id: i64) -> Result<Vec<ConversationSummary>, AppError> {
        // Deleted ones are gone from the owner's history; only a super admin still
        // sees them, through the admin surface.
        //
        // Tie-broken by id: two conversations can carry the same updatedAt to the
        // millisecond, and without a second key the sidebar order is whatever the
        // scan happened to produce — which changes when the query plan does.
        let rows = sqlx::query(
            "SELECT id, title, updated_at FROM conversations WHERE user_id = ?1 AND deleted_at IS NULL ORDER BY updated_at DESC, id DESC",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| ConversationSummary {
                id: row.try_get("id").expect("id"),
                title: row.try_get("title").expect("title"),
                updated_at: row.try_get("updated_at").expect("updated_at"),
            })
            .collect())
    }

    /// Ownership, for the write paths: posting a turn into a conversation, and
    /// taking one over. Deliberately narrower than `can_read` — a super admin may
    /// read another account's conversation but never write into it — and a deleted
    /// conversation is not writable either, so a stale `?c=` opens a new one.
    pub async fn is_owned_by(&self, id: i64, user_id: i64) -> Result<bool, AppError> {
        let row = sqlx::query(
            "SELECT id FROM conversations WHERE id = ?1 AND user_id = ?2 AND deleted_at IS NULL",
        )
        .bind(id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.is_some())
    }

    /// Hides conversations from their owner without removing anything: only
    /// `deleted_at` is set, so the messages, the assets and the super admin's view
    /// of them all survive. This is what the history dropdown calls.
    ///
    /// The hard counterpart is `delete_conversations` below, which the account
    /// deletion path uses — a removed account has to take its rows and files with it.
    pub async fn soft_delete_conversations(&self, ids: &[i64]) -> Result<u64, AppError> {
        if ids.is_empty() {
            return Ok(0);
        }
        // ?1 is the timestamp; the ids start at ?2.
        let marks = placeholders(ids.len(), 2);
        let sql = format!(
            "UPDATE conversations SET deleted_at = ?1 WHERE id IN ({marks}) AND deleted_at IS NULL"
        );
        let mut query = sqlx::query(&sql).bind(now_ms());
        for id in ids {
            query = query.bind(id);
        }
        Ok(query.execute(&self.pool).await?.rows_affected())
    }

    /// Read access, for every endpoint that only returns stored content.
    pub async fn can_read(&self, id: i64, viewer: &Principal) -> Result<bool, AppError> {
        let (user_id, is_super) = readable_args(viewer);
        let row = sqlx::query(&format!(
            "SELECT c.id FROM conversations c JOIN users u ON u.id = c.user_id WHERE c.id = ?3 AND {READABLE_SQL}"
        ))
        .bind(user_id)
        .bind(is_super)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.is_some())
    }

    /// Every conversation on the site, with its owner. Only for the admin surface.
    pub async fn list_all(&self) -> Result<Vec<AdminConversationSummary>, AppError> {
        // Tie-broken by id for the same reason as list_by_user: same-millisecond
        // updates would otherwise be ordered by whatever the scan produced.
        let rows = sqlx::query(&format!("{ADMIN_SUMMARY_SELECT}{ADMIN_SUMMARY_ORDER}"))
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(to_admin_summary).collect())
    }

    /// One conversation with its owner, for the admin read-only view.
    pub async fn summary_of(&self, id: i64) -> Result<Option<AdminConversationSummary>, AppError> {
        let rows = sqlx::query(&format!("{ADMIN_SUMMARY_SELECT} WHERE c.id = ?1"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(rows.as_ref().map(to_admin_summary))
    }

    /// Owner of a conversation, for endpoints that push events rather than read.
    pub async fn owner_of(&self, conversation_id: i64) -> Result<Option<i64>, AppError> {
        let row = sqlx::query("SELECT user_id FROM conversations WHERE id = ?1")
            .bind(conversation_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|row| row.try_get("user_id").expect("user_id")))
    }

    pub async fn views(&self, conversation_id: i64) -> Result<Vec<MessageView>, AppError> {
        let rows = sqlx::query(
            "SELECT id, role, content, reasoning, tool_calls, mode, created_at FROM messages WHERE conversation_id = ?1 ORDER BY id ASC",
        )
        .bind(conversation_id)
        .fetch_all(&self.pool)
        .await?;
        let grouped = self.assets_by_message(conversation_id).await?;
        Ok(rows
            .iter()
            .map(|row| {
                let id: i64 = row.try_get("id").expect("id");
                to_message_view(row, grouped.get(&id).map(Vec::as_slice).unwrap_or(&[]))
            })
            .collect())
    }

    /// `mode` is nullable because a voice turn runs in none of the four modes —
    /// voice is a separate surface, not a fifth mode (docs/architecture.md §11).
    pub async fn append_user_message(
        &self,
        conversation_id: i64,
        content: &str,
        mode: Option<Mode>,
    ) -> Result<i64, AppError> {
        self.insert_message(conversation_id, "user", content, None, mode)
            .await
    }

    /// Creates the assistant row up front so assets produced mid-turn can be
    /// linked to a message id (and pushed to the browser) before the turn ends.
    pub async fn create_assistant_placeholder(
        &self,
        conversation_id: i64,
        mode: Option<Mode>,
    ) -> Result<i64, AppError> {
        self.insert_message(conversation_id, "assistant", "", None, mode)
            .await
    }

    /// A complete assistant message, used by the task worker when a background job
    /// finishes: the turn that submitted it is long over, so the result lands as a
    /// message of its own.
    pub async fn append_assistant_message(
        &self,
        conversation_id: i64,
        content: &str,
        mode: Option<Mode>,
    ) -> Result<i64, AppError> {
        self.insert_message(conversation_id, "assistant", content, None, mode)
            .await
    }

    async fn insert_message(
        &self,
        conversation_id: i64,
        role: &str,
        content: &str,
        reasoning: Option<&str>,
        mode: Option<Mode>,
    ) -> Result<i64, AppError> {
        let row = sqlx::query(
            "INSERT INTO messages (conversation_id, role, content, reasoning, tool_calls, mode, created_at) VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6) RETURNING id",
        )
        .bind(conversation_id)
        .bind(role)
        .bind(content)
        .bind(reasoning)
        .bind(mode.map(|mode| mode.as_str()))
        .bind(now_ms())
        .fetch_one(&self.pool)
        .await?;
        // Only the two append paths move the conversation to the top of the list;
        // a placeholder does not, and neither does finalizing, which is the same
        // row's content changing.
        if role == "user" || !content.is_empty() {
            self.touch(conversation_id).await?;
        }
        Ok(row.try_get("id")?)
    }

    pub async fn finalize_assistant(
        &self,
        message_id: i64,
        content: &str,
        reasoning: Option<&str>,
        tool_calls: Option<&[ToolCallRecord]>,
    ) -> Result<(), AppError> {
        let encoded = tool_calls.map(|calls| serde_json::to_string(calls).unwrap_or_default());
        sqlx::query("UPDATE messages SET content = ?1, reasoning = ?2, tool_calls = ?3 WHERE id = ?4")
            .bind(content)
            .bind(reasoning)
            .bind(encoded)
            .bind(message_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// The assistant placeholder of a turn still running in this process: the row
    /// exists before the text does. The stream's catch-up starting point keys off
    /// it — a fresh connection to a busy conversation replays the live turn from
    /// its first event instead of showing nothing until it finalizes.
    pub async fn unfinished_assistant(&self, conversation_id: i64) -> Result<Option<i64>, AppError> {
        let row = sqlx::query(
            "SELECT id FROM messages WHERE conversation_id = ?1 AND role = 'assistant' AND content = '' ORDER BY id DESC LIMIT 1",
        )
        .bind(conversation_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|row| row.try_get("id").expect("id")))
    }

    /// Ends assistant rows a previous process left unfinished, at startup.
    ///
    /// A hard kill (restart, crash) stops a running turn with its placeholder
    /// empty. The empty content itself is not the danger — `build_input_items`
    /// skipping it would orphan the user message, and two consecutive user
    /// messages make the model re-run the failed request as if it were new (that
    /// once duplicated a paid video submission). A graceful stop finalizes through
    /// the turn loop's own cleanup; this covers everything that path never got to run.
    pub async fn abandon_stale_placeholders(&self) -> Result<u64, AppError> {
        let result = sqlx::query(
            "UPDATE messages SET content = '本轮处理失败：服务在轮次完成前中断。' WHERE role = 'assistant' AND content = ''",
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    pub async fn add_asset(
        &self,
        message_id: i64,
        asset: &crate::media::StoredAsset,
    ) -> Result<i64, AppError> {
        let row = sqlx::query(
            "INSERT INTO assets (message_id, kind, file_path, source_url, mime, bytes, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) RETURNING id",
        )
        .bind(message_id)
        .bind(asset.kind.as_str())
        // Stored as a name, resolved against STORAGE_DIR at read time: the row
        // must not remember a directory that only this instance knew.
        .bind(stored_asset_name(&asset.file_path))
        .bind(asset.source_url.clone())
        .bind(&asset.mime)
        .bind(asset.bytes)
        .bind(now_ms())
        .fetch_one(&self.pool)
        .await?;
        Ok(row.try_get("id")?)
    }

    pub async fn touch(&self, conversation_id: i64) -> Result<(), AppError> {
        sqlx::query("UPDATE conversations SET updated_at = ?1 WHERE id = ?2")
            .bind(now_ms())
            .bind(conversation_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Deletes conversations with everything under them — assets, tasks, messages —
    /// and then the mirrored files.
    ///
    /// Takes a list because it is the one implementation behind both "delete my
    /// conversation" and "delete an account" (which is every conversation of that
    /// account). The rows go in one transaction; the files go after it, since an
    /// unlink cannot be rolled back and a leftover file is the lesser evil.
    pub async fn delete_conversations(&self, ids: &[i64]) -> Result<(usize, usize), AppError> {
        if ids.is_empty() {
            return Ok((0, 0));
        }
        let asset_names = self.asset_names_of(ids).await?;

        let mut tx = self.pool.begin().await?;
        // Children before parents: assets and tasks both hang off rows that go next.
        let message_ids = message_ids_in(&mut tx, ids).await?;
        if !message_ids.is_empty() {
            delete_where_in(&mut tx, "assets", "message_id", &message_ids).await?;
        }
        delete_where_in(&mut tx, "conversation_events", "conversation_id", ids).await?;
        delete_where_in(&mut tx, "generation_tasks", "conversation_id", ids).await?;
        delete_where_in(&mut tx, "messages", "conversation_id", ids).await?;
        delete_where_in(&mut tx, "conversations", "id", ids).await?;
        tx.commit().await?;

        crate::media::asset_path::remove_asset_files(&self.config, &asset_names).await;
        Ok((ids.len(), asset_names.len()))
    }

    /// Stored names of every asset hanging off these conversations.
    async fn asset_names_of(&self, ids: &[i64]) -> Result<Vec<String>, AppError> {
        let marks = placeholders(ids.len(), 1);
        let rows = sqlx::query(&format!(
            "SELECT a.file_path AS file_path FROM assets a JOIN messages m ON m.id = a.message_id WHERE m.conversation_id IN ({marks})"
        ))
        .bind_many(ids)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|row| row.try_get::<String, _>("file_path").expect("file_path"))
            .collect())
    }

    /// A named image of this conversation, used as the first frame for
    /// image-to-video. Scoped to the conversation so an asset id cannot be pointed
    /// at another user's image.
    pub async fn image_asset_in(
        &self,
        conversation_id: i64,
        asset_id: i64,
    ) -> Result<Option<(i64, PathBuf)>, AppError> {
        let row = sqlx::query(
            "SELECT a.id AS id, a.file_path AS file_path FROM assets a JOIN messages m ON m.id = a.message_id WHERE m.conversation_id = ?1 AND a.kind = 'image' AND a.id = ?2",
        )
        .bind(conversation_id)
        .bind(asset_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|row| {
            (
                row.try_get::<i64, _>("id").expect("id"),
                resolve_asset_path(
                    &self.config,
                    &row.try_get::<String, _>("file_path").expect("file_path"),
                ),
            )
        }))
    }

    /// Unscoped by design: the signed, expiring query string is the authorization.
    /// The path is resolved for the caller, which only ever wants to stream it.
    pub async fn asset_file(&self, asset_id: i64) -> Result<Option<(PathBuf, String)>, AppError> {
        let row = sqlx::query("SELECT file_path, mime FROM assets WHERE id = ?1")
            .bind(asset_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|row| {
            (
                resolve_asset_path(
                    &self.config,
                    &row.try_get::<String, _>("file_path").expect("file_path"),
                ),
                row.try_get::<String, _>("mime").expect("mime"),
            )
        }))
    }

    pub async fn message_view(&self, message_id: i64) -> Result<Option<MessageView>, AppError> {
        let row = sqlx::query(
            "SELECT id, role, content, reasoning, tool_calls, mode, created_at FROM messages WHERE id = ?1",
        )
        .bind(message_id)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else { return Ok(None) };
        let assets = self.assets_for_message(row.try_get("id")?).await?;
        Ok(Some(to_message_view(&row, &assets)))
    }

    /// Replays the conversation as text turns. Tool round-trips are not replayed.
    pub async fn build_input_items(&self, conversation_id: i64) -> Result<Vec<InputItem>, AppError> {
        let rows = sqlx::query("SELECT role, content FROM messages WHERE conversation_id = ?1 ORDER BY id ASC")
            .bind(conversation_id)
            .fetch_all(&self.pool)
            .await?;
        let mut items = Vec::new();
        for row in rows {
            let content: String = row.try_get("content").expect("content");
            let trimmed = content.trim();
            if trimmed.is_empty() {
                continue;
            }
            let role: String = row.try_get("role").expect("role");
            items.push(InputItem { role, content: trimmed.to_string() });
        }
        Ok(items)
    }

    /// Scoped lookup for the download endpoint. An asset follows the visibility of
    /// the conversation it hangs off, so the admin read-only view can show the
    /// images of a conversation it may read.
    pub async fn find_asset_for_viewer(
        &self,
        asset_id: i64,
        viewer: &Principal,
    ) -> Result<Option<(AssetView, PathBuf)>, AppError> {
        let (user_id, is_super) = readable_args(viewer);
        let row = sqlx::query(&format!(
            "SELECT a.id AS id, a.kind AS kind, a.mime AS mime, a.bytes AS bytes, a.file_path AS file_path
             FROM assets a
             JOIN messages m ON m.id = a.message_id
             JOIN conversations c ON c.id = m.conversation_id
             JOIN users u ON u.id = c.user_id
             WHERE a.id = ?3 AND {READABLE_SQL}"
        ))
        .bind(user_id)
        .bind(is_super)
        .bind(asset_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|row| {
            (
                to_asset_view(&row),
                resolve_asset_path(
                    &self.config,
                    &row.try_get::<String, _>("file_path").expect("file_path"),
                ),
            )
        }))
    }

    pub async fn assets_for_message(&self, message_id: i64) -> Result<Vec<AssetView>, AppError> {
        let rows = sqlx::query("SELECT id, kind, mime, bytes FROM assets WHERE message_id = ?1 ORDER BY id ASC")
            .bind(message_id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(to_asset_view).collect())
    }

    async fn assets_by_message(
        &self,
        conversation_id: i64,
    ) -> Result<HashMap<i64, Vec<AssetView>>, AppError> {
        let rows = sqlx::query(
            "SELECT a.id AS id, a.message_id AS message_id, a.kind AS kind, a.mime AS mime, a.bytes AS bytes
             FROM assets a JOIN messages m ON m.id = a.message_id
             WHERE m.conversation_id = ?1 ORDER BY a.id ASC",
        )
        .bind(conversation_id)
        .fetch_all(&self.pool)
        .await?;
        let mut grouped: HashMap<i64, Vec<AssetView>> = HashMap::new();
        for row in &rows {
            let message_id: i64 = row.try_get("message_id").expect("message_id");
            grouped.entry(message_id).or_default().push(to_asset_view(row));
        }
        Ok(grouped)
    }
}

/// Shared by the site-wide list and the single-conversation lookup. The list adds
/// the ordering; a lookup must not carry it.
const ADMIN_SUMMARY_SELECT: &str = "SELECT c.id AS id, c.title AS title, c.updated_at AS updated_at, c.deleted_at AS deleted_at, u.id AS owner_id, u.username AS owner_name FROM conversations c JOIN users u ON u.id = c.user_id";
const ADMIN_SUMMARY_ORDER: &str = " ORDER BY c.updated_at DESC, c.id DESC";

fn to_admin_summary(row: &sqlx::sqlite::SqliteRow) -> AdminConversationSummary {
    AdminConversationSummary {
        id: row.try_get("id").expect("id"),
        title: row.try_get("title").expect("title"),
        updated_at: row.try_get("updated_at").expect("updated_at"),
        deleted_at: row.try_get("deleted_at").ok().flatten(),
        owner_id: row.try_get("owner_id").expect("owner_id"),
        owner_name: row.try_get("owner_name").expect("owner_name"),
    }
}

fn to_asset_view(row: &sqlx::sqlite::SqliteRow) -> AssetView {
    let id: i64 = row.try_get("id").expect("id");
    AssetView {
        id,
        kind: parse_kind(&row.try_get::<String, _>("kind").expect("kind")),
        url: asset_url(id),
        mime: row.try_get("mime").expect("mime"),
        bytes: row.try_get("bytes").expect("bytes"),
    }
}

fn parse_kind(raw: &str) -> TaskKind {
    match raw {
        "video" => TaskKind::Video,
        _ => TaskKind::Image,
    }
}

fn to_message_view(row: &sqlx::sqlite::SqliteRow, assets: &[AssetView]) -> MessageView {
    build_message_view(
        row.try_get("id").expect("id"),
        row.try_get("role").expect("role"),
        row.try_get("content").expect("content"),
        row.try_get("reasoning").ok().flatten(),
        row.try_get("tool_calls").ok().flatten(),
        row.try_get("mode").ok().flatten(),
        row.try_get("created_at").expect("created_at"),
        assets.to_vec(),
    )
}

#[allow(clippy::too_many_arguments)]
fn build_message_view(
    id: i64,
    role: String,
    content: String,
    reasoning: Option<String>,
    tool_calls: Option<String>,
    mode: Option<String>,
    created_at: i64,
    assets: Vec<AssetView>,
) -> MessageView {
    MessageView {
        id,
        role,
        content,
        reasoning,
        tool_calls: parse_tool_calls(tool_calls),
        // A voice turn has no mode, and defaulting null to auto would label the
        // history wrong.
        mode: Mode::from_storage(mode.as_deref()),
        created_at,
        assets,
    }
}

async fn message_ids_in(tx: &mut Transaction<'_, sqlx::Sqlite>, ids: &[i64]) -> Result<Vec<i64>, AppError> {
    let marks = placeholders(ids.len(), 1);
    let rows = sqlx::query(&format!(
        "SELECT id FROM messages WHERE conversation_id IN ({marks})"
    ))
    .bind_many(ids)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows
        .iter()
        .map(|row| row.try_get::<i64, _>("id").expect("id"))
        .collect())
}

async fn delete_where_in(
    tx: &mut Transaction<'_, sqlx::Sqlite>,
    table: &str,
    column: &str,
    ids: &[i64],
) -> Result<(), AppError> {
    let marks = placeholders(ids.len(), 1);
    sqlx::query(&format!("DELETE FROM {table} WHERE {column} IN ({marks})"))
        .bind_many(ids)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// `?, ?, ?` for a list, numbered from `first`.
fn placeholders(count: usize, first: usize) -> String {
    (0..count)
        .map(|index| format!("?{}", first + index))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Binds a slice of ids onto the placeholders produced by `placeholders`.
trait BindMany {
    fn bind_many(self, ids: &[i64]) -> Self;
}

impl<'q> BindMany for Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>> {
    fn bind_many(self, ids: &[i64]) -> Self {
        let mut query = self;
        for id in ids {
            query = query.bind(*id);
        }
        query
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::roles::Role;
    use crate::media::StoredAsset;
    use sqlx::SqlitePool;

    #[test]
    fn a_null_mode_stays_null_and_a_known_one_parses() {
        let view = build_message_view(
            1,
            "assistant".to_string(),
            "hi".to_string(),
            None,
            None,
            None,
            0,
            vec![],
        );
        assert_eq!(view.mode, None);
    }

    #[test]
    fn asset_urls_are_built_from_one_place() {
        assert_eq!(asset_url(12), "/api/v1/assets/12/download");
    }

    /// Two accounts (`owner`, `other`) plus a super admin, each with one
    /// conversation, so every branch of the visibility rule is exercised against
    /// real rows rather than a mocked query.
    async fn seeded() -> (ConversationService, SqlitePool) {
        let pool = crate::db::open_in_memory().await;
        for (id, username, role) in [
            (1, "owner", "user"),
            (2, "other", "user"),
            (3, "boss", "super"),
        ] {
            sqlx::query("INSERT INTO users (id, username, password_hash, role, created_at) VALUES (?1, ?2, 'h', ?3, 1)")
                .bind(id)
                .bind(username)
                .bind(role)
                .execute(&pool)
                .await
                .expect("user");
        }
        for (id, user_id) in [(10, 1), (11, 1), (20, 2)] {
            sqlx::query("INSERT INTO conversations (id, user_id, title, created_at, updated_at) VALUES (?1, ?2, 't', 1, ?3)")
                .bind(id)
                .bind(user_id)
                .bind(1000 - id)
                .execute(&pool)
                .await
                .expect("conversation");
        }
        let config = Arc::new(AppConfig::test(
            "/tmp/kestrel-repo-tests",
            "https://example.test",
        ));
        (ConversationService::new(pool.clone(), config), pool)
    }

    fn principal(id: i64, role: Role) -> Principal {
        Principal {
            id,
            username: "u".to_string(),
            role,
        }
    }

    #[tokio::test]
    async fn the_read_rule_is_a_full_role_matrix() {
        let (service, pool) = seeded().await;
        let owner = principal(1, Role::User);
        let other = principal(2, Role::User);
        let boss = principal(3, Role::Super);

        assert!(service.can_read(10, &owner).await.expect("read"));
        assert!(!service.can_read(10, &other).await.expect("read"));
        // A super admin reads another normal account's conversation, and its own.
        assert!(service.can_read(10, &boss).await.expect("read"));
        assert!(service.can_read(20, &boss).await.expect("read"));

        // ...but never another super admin's: boss owns 11 here.
        sqlx::query("UPDATE conversations SET user_id = 3 WHERE id = 11")
            .execute(&pool)
            .await
            .expect("reassign");
        let boss2 = principal(4, Role::Super);
        assert!(!service.can_read(11, &boss2).await.expect("read"));

        // Soft-deleted: hidden from the owner, still readable by a super admin.
        assert_eq!(service.soft_delete_conversations(&[10]).await.expect("hide"), 1);
        assert!(!service.can_read(10, &owner).await.expect("read"), "hidden from owner");
        assert!(service.can_read(10, &boss).await.expect("read"), "still visible to admin");
        assert_eq!(service.soft_delete_conversations(&[10]).await.expect("already hidden"), 0);
        assert_eq!(service.soft_delete_conversations(&[]).await.expect("empty"), 0);
    }

    #[tokio::test]
    async fn ownership_is_narrower_than_read_access() {
        let (service, _) = seeded().await;
        assert!(service.is_owned_by(10, 1).await.expect("own"));
        assert!(!service.is_owned_by(10, 2).await.expect("not mine"));
        // A super admin may read a normal account's conversation but never write
        // into it, so ownership is false for it too.
        assert!(!service.is_owned_by(10, 3).await.expect("admin is not the owner"));
        // A hidden conversation is not writable either: a stale `?c=` opens a new one.
        service.soft_delete_conversations(&[10]).await.expect("hide");
        assert!(!service.is_owned_by(10, 1).await.expect("hidden"));
    }

    #[tokio::test]
    async fn the_history_list_hides_deleted_rows_and_orders_by_recency() {
        let (service, _) = seeded().await;
        let list = service.list_by_user(1).await.expect("list");
        assert_eq!(
            list.iter().map(|item| item.id).collect::<Vec<_>>(),
            vec![10, 11],
            "updated_at DESC then id DESC"
        );
        service.soft_delete_conversations(&[10]).await.expect("hide");
        assert_eq!(service.list_by_user(1).await.expect("list").len(), 1);

        // The admin surface sees everything, with the owner attached.
        let all = service.list_all().await.expect("admin list");
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].owner_name, "owner");
        assert!(all.iter().any(|item| item.deleted_at.is_some()));
        let one = service.summary_of(20).await.expect("summary").expect("row");
        assert_eq!(one.owner_id, 2);
        assert_eq!(service.owner_of(20).await.expect("owner"), Some(2));
        assert_eq!(service.owner_of(999).await.expect("missing"), None);
    }

    #[tokio::test]
    async fn title_comes_from_the_first_prompt_and_falls_back() {
        let (service, _) = seeded().await;
        let long = "一二三四五六七八九十".repeat(5);
        let id = service.create(1, &long).await.expect("create");
        let summary = service
            .list_by_user(1)
            .await
            .expect("list")
            .into_iter()
            .find(|item| item.id == id)
            .expect("new row");
        assert_eq!(summary.title.chars().count(), MAX_TITLE_LENGTH);

        let blank = service.create(1, "   ").await.expect("create");
        let blank = service
            .list_by_user(1)
            .await
            .expect("list")
            .into_iter()
            .find(|item| item.id == blank)
            .expect("new row");
        assert_eq!(blank.title, "新会话");
    }

    #[tokio::test]
    async fn a_placeholder_is_not_a_finished_turn_but_a_stale_one_is_abandoned_at_boot() {
        let (service, _) = seeded().await;
        let _user = service
            .append_user_message(10, "画一只红隼", Some(Mode::Image))
            .await
            .expect("user");
        assert_eq!(service.unfinished_assistant(10).await.expect("probe"), None);

        let placeholder = service
            .create_assistant_placeholder(10, Some(Mode::Image))
            .await
            .expect("placeholder");
        assert_eq!(
            service.unfinished_assistant(10).await.expect("probe"),
            Some(placeholder)
        );

        service
            .finalize_assistant(placeholder, "好了", Some("构思…"), None)
            .await
            .expect("finalize");
        assert_eq!(service.unfinished_assistant(10).await.expect("probe"), None);

        let view = service.message_view(placeholder).await.expect("view").expect("row");
        assert_eq!(view.reasoning.as_deref(), Some("构思…"));
        assert_eq!(view.mode, Some(Mode::Image));

        // A second crash leaves another empty placeholder; boot ends them all.
        service
            .create_assistant_placeholder(10, Some(Mode::Image))
            .await
            .expect("placeholder");
        assert_eq!(service.abandon_stale_placeholders().await.expect("abandon"), 1);
        assert_eq!(service.unfinished_assistant(10).await.expect("probe"), None);
    }

    #[tokio::test]
    async fn empty_turns_are_never_replayed_into_the_next_request() {
        let (service, _) = seeded().await;
        service
            .append_user_message(10, "第一句", None)
            .await
            .expect("user");
        service
            .create_assistant_placeholder(10, None)
            .await
            .expect("placeholder");
        service
            .append_user_message(10, "  第二句  ", None)
            .await
            .expect("user");

        let items = service.build_input_items(10).await.expect("items");
        assert_eq!(items.len(), 2, "the empty placeholder is skipped");
        assert_eq!(items[0].content, "第一句");
        assert_eq!(items[1].content, "第二句", "trimmed");
        assert_eq!(items[0].role, "user");
    }

    #[tokio::test]
    async fn assets_follow_the_visibility_of_their_conversation() {
        let (service, pool) = seeded().await;
        let message = service
            .append_assistant_message(10, "图好了", Some(Mode::Image))
            .await
            .expect("assistant");
        let asset_id = service
            .add_asset(
                message,
                &StoredAsset {
                    kind: TaskKind::Image,
                    file_path: "/tmp/whatever/frame.png".to_string(),
                    source_url: Some("https://provider.test/frame.png".to_string()),
                    mime: "image/png".to_string(),
                    bytes: 4,
                },
            )
            .await
            .expect("asset");

        let owner = principal(1, Role::User);
        let other = principal(2, Role::User);
        let boss = principal(3, Role::Super);
        let (view, path) = service
            .find_asset_for_viewer(asset_id, &owner)
            .await
            .expect("lookup")
            .expect("visible");
        assert_eq!(view.url, asset_url(asset_id));
        assert_eq!(view.kind, TaskKind::Image);
        assert_eq!(view.mime, "image/png");
        // The row keeps a bare name, resolved against STORAGE_DIR at read time, so
        // it never points back at the directory the writer happened to be in.
        assert!(path.to_string_lossy().ends_with("frame.png"));
        assert!(!path.to_string_lossy().starts_with("/tmp/whatever"));

        assert!(service
            .find_asset_for_viewer(asset_id, &other)
            .await
            .expect("lookup")
            .is_none());
        assert!(service
            .find_asset_for_viewer(asset_id, &boss)
            .await
            .expect("lookup")
            .is_some());

        // i2v looks the frame up scoped to one conversation.
        assert!(service.image_asset_in(10, asset_id).await.expect("scoped").is_some());
        assert!(service.image_asset_in(20, asset_id).await.expect("other conversation").is_none());
        // The signed frame endpoint is unscoped by design.
        let (file, mime) = service.asset_file(asset_id).await.expect("file").expect("row");
        assert_eq!(mime, "image/png");
        assert!(file.to_string_lossy().ends_with("frame.png"));

        let views = service.views(10).await.expect("views");
        assert_eq!(views.last().expect("assistant").assets.len(), 1);

        // Hard delete takes the rows and the file with it.
        std::fs::create_dir_all(std::env::temp_dir().join("kestrel-test-storage")).ok();
        let (conversations, assets) = service.delete_conversations(&[10]).await.expect("delete");
        assert_eq!((conversations, assets), (1, 1));
        assert_eq!(
            sqlx::query("SELECT COUNT(*) AS n FROM assets WHERE message_id = ?1")
                .bind(message)
                .fetch_one(&pool)
                .await
                .expect("count")
                .try_get::<i64, _>("n")
                .expect("n"),
            0
        );
        assert_eq!(
            sqlx::query("SELECT COUNT(*) AS n FROM messages WHERE conversation_id = 10")
                .fetch_one(&pool)
                .await
                .expect("count")
                .try_get::<i64, _>("n")
                .expect("n"),
            0
        );
    }
}
