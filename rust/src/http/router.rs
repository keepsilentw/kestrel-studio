use axum::routing::{get, post};
use axum::Router;

use crate::admin::controller as admin;
use crate::auth::routes;
use crate::http::{asset_frame, chat, static_files};
use crate::view::controller as views;
use crate::state::AppState;

/// The route table. Kept in one place so the URL surface is reviewable at a
/// glance; the Nest version spread it over ten decorated controllers.
///
/// `asset_frame` sits outside the session guard on purpose - the signed query
/// string is its authorization (docs/architecture.md §4.4).
pub fn build(state: AppState) -> Router {
    Router::new()
        .route("/", get(views::chat))
        .route("/login", get(views::login_page).post(routes::login))
        .route("/voice", get(views::voice))
        .route("/api/voice", get(crate::voice::gateway::handle))
        .route("/api/voice/ticket", post(crate::voice::gateway::issue_ticket))
        .route("/admin", get(admin::index))
        .route("/admin/users", post(admin::create_user))
        .route("/admin/conversations", get(admin::conversations_page_handler))
        .route("/admin/conversations/{id}", get(admin::conversation))
        .route("/admin/users/{id}", get(admin::account))
        .route("/admin/users/{id}/delete", get(admin::confirm_delete).post(admin::remove_user))
        .route("/admin/users/{id}/password", post(admin::reset_password))
        .route("/logout", post(routes::logout))
        .route("/api/v1/conversations", post(chat::create_conversation).get(chat::list_conversations))
        .route("/api/v1/conversations/{id}/turns", post(chat::start_turn))
        .route("/api/v1/conversations/{id}/delete", post(chat::delete_conversation))
        .route("/api/v1/conversations/{id}/messages", get(chat::conversation_messages))
        .route("/api/v1/conversations/{id}/tasks", get(chat::active_tasks))
        .route("/api/v1/conversations/{id}/stream", get(chat::stream))
        .route("/api/v1/assets/{id}/download", get(chat::download))
        .route("/api/v1/assets/{id}/frame", get(asset_frame::frame))
        .fallback(get(static_files::fallback))
        .with_state(state)
}
