use std::collections::HashMap;
use std::path::Path;

use axum::extract::{Path as UrlPath, Query, State};
use axum::http::header::{
    CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_TYPE, HeaderMap, HeaderValue,
};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::io::ReaderStream;

use crate::agent::mode::Mode;
use crate::agent::turn_runner::TurnRunner;
use crate::auth::guards::Authed;
use crate::auth::roles::is_super_admin;
use crate::common::args::read_string;
use crate::common::http::{read_non_negative_int, read_positive_int};
use crate::error::AppError;
use crate::sse;
use crate::state::AppState;

/// The product API (`/api/v1`), one surface for every client — the web page
/// today, App and mini-program later.
///
/// Two rules it is built around:
///
/// - Submit and subscribe are separate calls. `POST .../turns` answers 202
///   immediately; the turn runs in the process and its events land on the
///   conversation journal. `GET .../stream` is the only stream, resumable via
///   Last-Event-ID over the journal's `seq`.
/// - Read endpoints gate on `can_read` and write endpoints on `is_owned_by`: a
///   super admin may look at any normal account's conversation, but never post
///   into one.
///
/// Voice stays on its own entrance: a continuous two-way socket is not a turn.

/// Opens a conversation. `prompt` only seeds the title from its prefix.
pub async fn create_conversation(
    Authed(user): Authed,
    State(state): State<AppState>,
    body: Json<Value>,
) -> Result<Response, AppError> {
    let prompt = read_string(&body, "prompt", "");
    let conversation_id = state.conversations.create(user.id, &prompt).await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "conversationId": conversation_id })),
    )
        .into_response())
}

/// Submits one turn and returns as soon as its rows exist. Nothing here streams:
/// the answer arrives on `GET .../stream`, like every other event, which is what
/// lets the turn survive the client going away.
///
/// The assistant message id is the turn's identity — `turn_started`, every delta,
/// the assets and `turn_done` all carry it.
pub async fn start_turn(
    Authed(user): Authed,
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
    body: Json<Value>,
) -> Result<Response, AppError> {
    let prompt = read_string(&body, "prompt", "");
    if prompt.is_empty() {
        return Err(AppError::bad_request("prompt 不能为空"));
    }
    let Some(conversation_id) = read_positive_int(Some(&id)) else {
        return Err(AppError::not_found("not found"));
    };
    // 404 rather than the old silent fallback to a fresh conversation: with the id
    // in the path, inventing a different one would let a stale `?c=` (deleted in
    // another tab) write a turn the user meant elsewhere. The client recreates
    // explicitly.
    if !state
        .conversations
        .is_owned_by(conversation_id, user.id)
        .await?
    {
        return Err(AppError::not_found("not found"));
    }

    let mode = Mode::parse(body.get("mode").and_then(Value::as_str));
    let message_id = TurnRunner::new(state.clone())
        .start(conversation_id, &prompt, mode)
        .await?;
    let Some(message_id) = message_id else {
        // One turn per conversation: a second submit would double-bill the first.
        return Err(AppError::conflict("该会话已有轮次在进行中"));
    };
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({ "conversationId": conversation_id, "messageId": message_id })),
    )
        .into_response())
}

/// Hides one of the caller's own conversations from their history.
///
/// Soft on purpose: the messages, the assets and the files all stay, and a super
/// admin keeps reading the conversation — which is exactly why this is offered to
/// normal accounts only. A conversation that is not the caller's reads as "not
/// found" rather than "forbidden", so the endpoint does not confirm that it
/// exists; the role refusal is a rule about the caller, so it says so.
pub async fn delete_conversation(
    Authed(user): Authed,
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> Result<Response, AppError> {
    let Some(conversation_id) = read_positive_int(Some(&id)) else {
        return Err(AppError::not_found("not found"));
    };
    if is_super_admin(Some(&user)) {
        return Err(AppError::forbidden("超级管理员不提供会话删除"));
    }
    if !state
        .conversations
        .is_owned_by(conversation_id, user.id)
        .await?
    {
        return Err(AppError::not_found("not found"));
    }
    state
        .conversations
        .soft_delete_conversations(&[conversation_id])
        .await?;
    Ok(Json(json!({ "ok": true })).into_response())
}

pub async fn list_conversations(
    Authed(user): Authed,
    State(state): State<AppState>,
) -> Result<Response, AppError> {
    let list = state.conversations.list_by_user(user.id).await?;
    Ok(Json(json!(list)).into_response())
}

/// A conversation the caller may not read answers as an empty list, the same way
/// the Nest controller did: the drawer treats "no history" and "not yours"
/// identically, and a 404 would leak that the id exists.
pub async fn conversation_messages(
    Authed(user): Authed,
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> Result<Response, AppError> {
    let views = match read_positive_int(Some(&id)) {
        Some(conversation_id) if state.conversations.can_read(conversation_id, &user).await? => {
            state.conversations.views(conversation_id).await?
        }
        _ => Vec::new(),
    };
    Ok(Json(json!(views)).into_response())
}

pub async fn active_tasks(
    Authed(user): Authed,
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> Result<Response, AppError> {
    let rows: Vec<Value> = match read_positive_int(Some(&id)) {
        Some(conversation_id) if state.conversations.can_read(conversation_id, &user).await? => {
            state
                .tasks
                .active_by_conversation(conversation_id)
                .await?
                .iter()
                .map(|task| {
                    json!({
                        "id": task.id,
                        "kind": task.kind.as_str(),
                        "status": task.status.as_str(),
                    })
                })
                .collect()
        }
        _ => Vec::new(),
    };
    Ok(Json(json!(rows)).into_response())
}

/// The conversation-level stream: journaled events, resumable.
///
/// Every frame that carries an event also carries `id: <seq>` (the journal row
/// id), so an EventSource reconnect sends the cursor back as `Last-Event-ID` and
/// the missed events replay. A first connection knows where it stands through
/// `?from=`: the seq the page was painted at, or 0 for a conversation it just
/// created and wants from the start.
///
/// With neither cursor the stream catches up: it replays a still-running turn from
/// its first event and otherwise starts live — SSR already drew the finished
/// history.
///
/// A cursor older than the retained window yields one `resync` frame instead of a
/// partial replay that would look complete; the client rebuilds from the persisted
/// rows.
pub async fn stream(
    Authed(user): Authed,
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    let Some(conversation_id) = read_positive_int(Some(&id)) else {
        return Err(AppError::not_found("not found"));
    };
    if !state.conversations.can_read(conversation_id, &user).await? {
        return Err(AppError::not_found("not found"));
    }

    let last_event_id = headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok());
    let requested = read_non_negative_int(last_event_id)
        .or_else(|| read_non_negative_int(params.get("from").map(String::as_str)));

    let cursor = match requested {
        Some(value) => value,
        None => {
            TurnRunner::new(state.clone())
                .catch_up_cursor(conversation_id)
                .await?
        }
    };
    let gap = match requested {
        Some(value) => state.events.has_gap(conversation_id, value).await?,
        None => false,
    };

    // Subscribe before replaying. The Nest version could order this the other way
    // round because publishing inserted into the journal before fanning out, on
    // one thread; here the two halves run on different tasks, so the live tail is
    // opened first and the `seq > last_written` guard drops whatever the replay
    // already covered.
    let mut live = state.events.subscribe(conversation_id);
    let replay = state.events.replay_after(conversation_id, cursor).await?;
    // The highest seq the replay below covers, so the live tail can skip what it
    // already delivered instead of trusting arrival order.
    let replayed_through = replay.last().map(|published| published.seq);

    let (sender, receiver) = tokio::sync::mpsc::channel::<String>(256);
    let cleanup = state.clone();
    let handshake = if gap {
        vec![
            sse::frame(
                sse::RESYNC,
                &json!({ "conversationId": conversation_id, "cursor": requested.unwrap_or(cursor) }),
                None,
            ),
            sse::frame(sse::CONNECTED, &json!({ "conversationId": conversation_id }), None),
        ]
    } else {
        vec![sse::frame(
            sse::CONNECTED,
            &json!({ "conversationId": conversation_id }),
            None,
        )]
    };

    tokio::spawn(async move {
        let sender = sender;
        let mut last_written = replayed_through.unwrap_or(cursor);
        // Replay frames carry the journal's own bytes; live frames carry what was
        // journaled when they were published, so both are the same string.
        let frames = handshake.into_iter().chain(replay.into_iter().map(|published| {
            sse::raw_frame(published.event.type_name(), &published.payload, Some(published.seq))
        }));
        for frame in frames {
            if sender.send(frame).await.is_err() {
                cleanup.events.release(conversation_id);
                return;
            }
        }

        loop {
            tokio::select! {
                _ = sender.closed() => break,
                next = live.recv() => match next {
                    Ok(published) => {
                        if published.seq <= last_written {
                            continue;
                        }
                        last_written = published.seq;
                        let frame = sse::raw_frame(
                            published.event.type_name(),
                            &published.payload,
                            Some(published.seq),
                        );
                        if sender.send(frame).await.is_err() {
                            break;
                        }
                    }
                    // The journal's sender went away: every reader left, so nothing
                    // will be published on this conversation until a new turn opens
                    // one. Ending the stream is honest — the client reconnects.
                    Err(_) => break,
                }
            }
        }
        drop(live);
        cleanup.events.release(conversation_id);
    });

    Ok(sse::response(ReceiverStream::new(receiver)))
}

/// Image generation URLs are provider-side and expire; serve from disk instead.
pub async fn download(
    Authed(user): Authed,
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> Result<Response, AppError> {
    let Some(asset_id) = read_positive_int(Some(&id)) else {
        return Err(AppError::not_found("not found"));
    };
    let Some((asset, file_path)) = state
        .conversations
        .find_asset_for_viewer(asset_id, &user)
        .await?
    else {
        return Err(AppError::not_found("not found"));
    };
    serve_file(&file_path, &asset.mime, disposition(&file_path), "public, max-age=0").await
}

/// Streams a stored asset. `Content-Length` comes from the file itself so the
/// browser can show progress on a multi-megabyte video.
pub(crate) async fn serve_file(
    path: &Path,
    mime: &str,
    disposition: Option<String>,
    cache: &str,
) -> Result<Response, AppError> {
    let meta = tokio::fs::metadata(path)
        .await
        .map_err(|_| AppError::not_found("not found"))?;
    if !meta.is_file() {
        return Err(AppError::not_found("not found"));
    }
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|_| AppError::not_found("not found"))?;

    let mut headers = HeaderMap::new();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_str(mime)
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    headers.insert(
        CACHE_CONTROL,
        HeaderValue::from_str(cache).unwrap_or_else(|_| HeaderValue::from_static("no-cache")),
    );
    headers.insert(CONTENT_LENGTH, HeaderValue::from(meta.len()));
    if let Some(value) = disposition {
        headers.insert(
            CONTENT_DISPOSITION,
            HeaderValue::from_str(&value).unwrap_or_else(|_| HeaderValue::from_static("inline")),
        );
    }

    Ok((
        StatusCode::OK,
        headers,
        axum::body::Body::from_stream(ReaderStream::new(file)),
    )
        .into_response())
}

fn disposition(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_string_lossy().into_owned();
    Some(format!("inline; filename=\"{name}\""))
}
