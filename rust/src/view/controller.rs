use std::collections::HashMap;

use axum::extract::{Query, State};
use axum::http::header::{HeaderMap, CONTENT_TYPE};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::auth::guards::{principal_from_cookie, ViewAuthed};
use crate::common::http::read_positive_int;
use crate::error::AppError;
use crate::http::redirect::found;
use crate::state::AppState;

use super::render::{build_turn_anchors, render_messages};
use super::templates::{ChatPage, ConversationEntry};

/// SSR renders the first paint only: login state, the history sidebar, the page
/// shell and the mode picker. From the first token on, the main column is built by
/// the client - the two render paths do not overlap, and nothing here streams.

const HTML: &str = "text/html; charset=utf-8";

fn html(body: String) -> Response {
    (
        StatusCode::OK,
        [(CONTENT_TYPE, HeaderValue::from_static(HTML))],
        body,
    )
        .into_response()
}

/// `GET /login` — the page itself. A signed-in visitor is bounced to the chat,
/// which is what the Nest controller did before rendering anything.
pub async fn login_page(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let cookie = headers
        .get(axum::http::header::COOKIE)
        .and_then(|value| value.to_str().ok());
    if principal_from_cookie(&state, cookie).await.is_some() {
        return found("/");
    }
    html(super::templates::login_page(params.get("error").map(String::as_str) == Some("1")))
}

/// `GET /` — the first paint of one conversation, or of an empty shell.
pub async fn chat(
    ViewAuthed(user): ViewAuthed,
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    let list = state.conversations.list_by_user(user.id).await?;
    let requested = read_positive_int(params.get("c").map(String::as_str));
    // A `?c=` that is not the caller's own live conversation is ignored, not an
    // error: the page falls back to the most recent one, and if there is none it
    // opens nothing.
    let active = match requested {
        Some(id) if state.conversations.is_owned_by(id, user.id).await? => Some(id),
        _ => list.first().map(|item| item.id),
    };

    let conversations: Vec<ConversationEntry> = list
        .iter()
        .map(|item| ConversationEntry {
            id: item.id,
            title: item.title.clone(),
            is_active: Some(item.id) == active,
        })
        .collect();

    let pending_tasks = match active {
        None => Vec::new(),
        Some(id) => state
            .tasks
            .active_by_conversation(id)
            .await?
            .iter()
            .map(|task| super::templates::PendingTask {
                id: task.id,
                kind: task.kind,
                status_label: task.status.label().to_string(),
            })
            .collect(),
    };

    let raw_views = match active {
        None => Vec::new(),
        Some(id) => state.conversations.views(id).await?,
    };
    // An assistant placeholder of a still-running turn is not painted: the stream
    // replays that turn from its first event (startSeq is set to before it), and a
    // half-drawn row would only double or contradict it.
    let painted: Vec<_> = raw_views
        .into_iter()
        .filter(|view| !(view.role == "assistant" && view.content.is_empty()))
        .collect();
    let messages = render_messages(painted);
    let turn_anchors = build_turn_anchors(&messages);

    let start_seq = match active {
        None => None,
        Some(id) => {
            Some(crate::agent::turn_runner::TurnRunner::new(state.clone()).catch_up_cursor(id).await?)
        }
    };

    // Deleting from the history list is a normal account's affordance: a super
    // admin sees everyone's history and does not hide its own from them.
    let page = ChatPage {
        username: user.username.clone(),
        is_super: user.role == crate::auth::roles::Role::Super,
        conversations,
        active_conversation_id: active,
        start_seq,
        messages,
        pending_tasks,
        turn_anchors,
        can_delete_conversations: !crate::auth::roles::is_super_admin(Some(&user)),
    };
    Ok(html(super::templates::chat_page(&page)))
}

/// `GET /voice` — the second entrance. No conversation is rendered: a call creates
/// its own on the server when the socket opens, and the client appends everything
/// from there.
pub async fn voice(ViewAuthed(user): ViewAuthed) -> Response {
    html(super::templates::voice_page(
        &user.username,
        user.role == crate::auth::roles::Role::Super,
    ))
}
