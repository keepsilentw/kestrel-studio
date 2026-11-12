use axum::routing::{delete, get, post};
use axum::Router;
use tower_http::trace::TraceLayer;

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
        .route("/api/voice/ticket", post(crate::voice::gateway::issue_ticket))
        .route("/admin", get(admin::index))
        .route("/admin/users", post(admin::create_user))
        .route("/admin/conversations", get(admin::conversations_page_handler))
        .route("/admin/conversations/{id}", get(admin::conversation))
        .route("/admin/users/{id}", get(admin::account))
        .route("/admin/users/{id}/delete", get(admin::confirm_delete).post(admin::remove_user))
        .route("/admin/users/{id}/password", post(admin::reset_password))
        .route("/logout", post(routes::logout))
        .route("/api/v1/session", post(routes::json_login))
        .route(
            "/api/v1/tokens",
            post(routes::create_token).get(routes::list_tokens),
        )
        .route("/api/v1/tokens/{id}", delete(routes::revoke_token))
        .route("/api/v1/conversations", post(chat::create_conversation).get(chat::list_conversations))
        .route("/api/v1/conversations/{id}/turns", post(chat::start_turn))
        .route("/api/v1/conversations/{id}/delete", post(chat::delete_conversation))
        .route("/api/v1/conversations/{id}/messages", get(chat::conversation_messages))
        .route("/api/v1/conversations/{id}/tasks", get(chat::active_tasks))
        .route("/api/v1/conversations/{id}/stream", get(chat::stream))
        .route("/api/v1/assets/{id}/download", get(chat::download))
        .route("/api/v1/assets/{id}/frame", get(asset_frame::frame))
        .fallback(get(static_files::fallback))
        // One access line per request, at DEBUG — that is tower-http's own default
        // for `make_span` / `on_request` / `on_response` (`on_failure` stays ERROR),
        // and `main.rs` runs the `EnvFilter` at `info`, so production log volume does
        // not change. A grey-release run sets `RUST_LOG=debug` and gets method, uri,
        // status and latency, which is the only way the read-only P1 criteria leave a
        // trace at all (docs/verification.md §13.6): the uri keeps its query string, so a
        // resumed stream is readable as `/api/v1/conversations/{id}/stream?from=N`.
        // Caveat for the two streaming routes: `finished processing request` fires when
        // the head goes out, so its latency is time-to-first-byte, not the call length.
        .layer(TraceLayer::new_for_http())
        // Deliberately added *after* the layer: axum only wraps routes registered
        // before `.layer()`. This is the one route that hijacks the connection for a
        // WebSocket, and its `hyper::upgrade::on` takeover has never been run on a
        // machine (docs/architecture.md §13.3), so nothing that touches the response
        // body gets put in front of it without evidence. A real call already logs
        // itself at INFO (`voice session opened`, `voice turn committed`), so the
        // access line would buy nothing here anyway. Move it inside the traced group
        // only after an on-host run proves the 101 still lands.
        .route("/api/voice", get(crate::voice::gateway::handle))
        .with_state(state)
}
