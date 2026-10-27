use axum::extract::{Query, Request, State};
use axum::http::header::{HeaderValue, CONNECTION, SEC_WEBSOCKET_ACCEPT, SEC_WEBSOCKET_KEY, SEC_WEBSOCKET_VERSION, UPGRADE};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use hyper_util::rt::TokioIo;

use crate::net::ws::{accept_key, encode, Frame};
use crate::state::AppState;


/// The voice transport's entrance: a WebSocket on the same origin as the pages.
///
/// `axum`'s `ws` feature is not available in this build (it pulls
/// `tokio-tungstenite`, which is not in the local registry), but the upgrade does
/// not need it: `hyper::upgrade::on` is generic over the request body type, so
/// the handler that already holds an `axum::extract::Request` can take over the
/// connection itself. That is what this module does.

/// A browser cannot set a header on a `WebSocket` handshake, so the credential is
/// a one-time ticket in the query string (`voice::ticket`). The name is `ticket`
/// because `web/scripts/voice.ts` builds the URL with it and is unchanged.
pub const TICKET_QUERY: &str = "ticket";

#[derive(Debug, PartialEq, Eq)]
pub enum Reject {
    NotAnUpgrade,
    BadVersion,
    MissingKey,
    /// The ticket was absent, used, or too old.
    Unauthorized,
}

impl Reject {
    fn into_response(self) -> Response {
        match self {
            // 426 says "upgrade required", which is the honest answer to a request
            // that arrived on this path without a WebSocket handshake.
            Self::NotAnUpgrade | Self::BadVersion => (
                StatusCode::UPGRADE_REQUIRED,
                "this endpoint speaks WebSocket",
            )
                .into_response(),
            Self::MissingKey => (StatusCode::BAD_REQUEST, "missing Sec-WebSocket-Key").into_response(),
            // 401, as in the Nest gateway: the socket is refused before it exists,
            // so this is authentication, not authorisation.
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "票据无效或已过期").into_response(),
        }
    }
}

fn header_flag(headers: &HeaderMap, name: &str, wanted: &str) -> bool {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .any(|part| part.trim().eq_ignore_ascii_case(wanted))
        })
}

/// Validates the handshake headers and returns the key to sign.
///
/// `Connection` is a comma list and both names are case-insensitive on the wire
/// (`keep-alive, Upgrade` is what a browser behind a proxy sends), so the check
/// splits rather than compares whole strings.
pub fn parse_upgrade(headers: &HeaderMap) -> Result<String, Reject> {
    if !header_flag(headers, "connection", "upgrade") || !header_flag(headers, "upgrade", "websocket") {
        return Err(Reject::NotAnUpgrade);
    }
    if headers.get(SEC_WEBSOCKET_VERSION).and_then(|value| value.to_str().ok()) != Some("13") {
        return Err(Reject::BadVersion);
    }
    headers
        .get(SEC_WEBSOCKET_KEY)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_string())
        .ok_or(Reject::MissingKey)
}

/// The 101 that switches the connection over.
fn switch_to_websocket(accept: &str) -> Response {
    Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header(UPGRADE, HeaderValue::from_static("websocket"))
        .header(CONNECTION, HeaderValue::from_static("Upgrade"))
        .header(
            SEC_WEBSOCKET_ACCEPT,
            HeaderValue::from_str(accept).expect("base64 is a valid header value"),
        )
        .body(axum::body::Body::empty())
        .expect("a 101 with no body")
}

pub async fn handle(
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
    headers: HeaderMap,
    request: Request,
) -> Response {
    let raw_key = match parse_upgrade(&headers) {
        Ok(key) => key,
        Err(reject) => return reject.into_response(),
    };
    let accept = match accept_key(&raw_key) {
        Some(value) => value,
        // The key is not a base64 value, so no signature we print would be
        // correct; say so instead of completing a handshake we cannot honour.
        None => return Reject::MissingKey.into_response(),
    };

    let now = chrono::Utc::now().timestamp_millis();
    let Some(user_id) = params
        .get(TICKET_QUERY)
        .and_then(|token| state.tickets.redeem(token, now))
    else {
        return Reject::Unauthorized.into_response();
    };
    // Both halves of the upgraded socket get a pump of their own, and the call
    // state machine owns the ordering between them.
    tokio::spawn(async move {
        let upgraded = match hyper::upgrade::on(request).await {
            Ok(io) => TokioIo::new(io),
            Err(error) => {
                tracing::warn!(%error, "the upgrade never completed");
                return;
            }
        };
        let (reader, writer) = tokio::io::split(upgraded);
        // Bounded but roomy: a stalled call must not grow memory without limit,
        // while 256 queued frames is more than the browser can have in flight.
        let (inbound_tx, inbound_rx) = tokio::sync::mpsc::channel(256);
        let (outbound_tx, mut outbound_rx) = tokio::sync::mpsc::channel(64);

        let reader_task = tokio::spawn(async move {
            crate::voice::relay::pump_in(&mut { reader }, &inbound_tx).await.ok();
        });
        let writer_task = tokio::spawn(async move {
            crate::voice::relay::pump_out(&mut { writer }, &mut outbound_rx).await.ok();
        });

        let call = crate::voice::session::VoiceCall::new(state, user_id, outbound_tx);
        call.run(inbound_rx).await;

        reader_task.abort();
        writer_task.abort();
    });

    switch_to_websocket(&accept)
}

/// `POST /api/voice/ticket` - trades the session for a single-use ticket.
///
/// The upgrade never reaches the session guard, so the browser spends this ticket
/// on the handshake instead. The body shape is what `web/scripts/voice.ts` reads.
pub async fn issue_ticket(
    crate::auth::guards::Authed(user): crate::auth::guards::Authed,
    State(state): State<AppState>,
) -> axum::Json<serde_json::Value> {
    let now = chrono::Utc::now().timestamp_millis();
    let ticket = state.tickets.issue(user.id, now);
    axum::Json(serde_json::json!({
        "ticket": ticket,
        "expiresAt": now + crate::voice::ticket::TICKET_TTL_MS,
    }))
}
