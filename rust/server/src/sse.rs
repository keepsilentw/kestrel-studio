use std::convert::Infallible;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use futures_util::Stream;
use serde_json::Value;

/// Event names carried by the conversation stream. Mirrors
/// docs/architecture.md §6. Everything except `connected` and `resync` is
/// journaled in `conversation_events` and therefore replayable on reconnect; the
/// two connection-scoped names never carry an id.
pub const CONNECTED: &str = "connected";
pub const RESYNC: &str = "resync";
pub const USER_MESSAGE: &str = "user_message";
pub const TURN_STARTED: &str = "turn_started";
pub const REASONING: &str = "reasoning";
pub const TEXT: &str = "text";
pub const TOOL_CALL: &str = "tool_call";
pub const TOOL_RESULT: &str = "tool_result";
pub const ASSET: &str = "asset";
pub const TURN_DONE: &str = "turn_done";
pub const TURN_ERROR: &str = "turn_error";
pub const TASK_UPDATED: &str = "task_updated";
pub const MESSAGE_ADDED: &str = "message_added";

const HEARTBEAT: Duration = Duration::from_secs(15);

/// `seq` is the journal id of a journaled event. It becomes the SSE `id:` field,
/// so a browser reconnecting an EventSource sends it back as Last-Event-ID and
/// the stream replays from there. Connection-scoped frames (`connected`,
/// `resync`) omit it — they must not move the client's cursor.
/// The same frame from data that is already JSON text - the journal's stored
/// payload, which is replayed verbatim rather than re-serialized.
pub fn raw_frame(event: &str, data: &str, seq: Option<i64>) -> String {
    let id = match seq {
        Some(value) => format!("id: {value}\n"),
        None => String::new(),
    };
    format!("{id}event: {event}\ndata: {data}\n\n")
}

pub fn frame(event: &str, data: &Value, seq: Option<i64>) -> String {
    let id = match seq {
        Some(value) => format!("id: {value}\n"),
        None => String::new(),
    };
    format!("{id}event: {event}\ndata: {}\n\n", serde_json::to_string(data).unwrap_or_else(|_| "\"{}\"".to_string()))
}

/// A stream that keeps its own comment frame on an interval: image generation
/// leaves the connection silent for tens of seconds, and without a heartbeat
/// proxies and browsers treat the stream as idle and close it.
pub struct WithHeartbeat<S> {
    inner: S,
    ticker: tokio::time::Interval,
    finished: bool,
}

impl<S> WithHeartbeat<S> {
    pub fn new(inner: S) -> Self {
        let mut ticker = tokio::time::interval(HEARTBEAT);
        // The first tick fires immediately, which would prepend a ping before
        // the `connected` frame. Skip that.
        ticker.reset();
        Self {
            inner,
            ticker,
            finished: false,
        }
    }
}

impl<S> Stream for WithHeartbeat<S>
where
    S: Stream<Item = String> + Unpin,
{
    type Item = Result<String, Infallible>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.finished {
            return Poll::Ready(None);
        }
        match Pin::new(&mut this.inner).poll_next(context) {
            Poll::Ready(Some(item)) => Poll::Ready(Some(Ok(item))),
            Poll::Ready(None) => {
                this.finished = true;
                Poll::Ready(None)
            }
            Poll::Pending => match this.ticker.poll_tick(context) {
                Poll::Ready(_) => Poll::Ready(Some(Ok(": ping\n\n".to_string()))),
                Poll::Pending => Poll::Pending,
            },
        }
    }
}

/// Opens an SSE response the way the express writer did: these headers, and a
/// body that stays open until the inner stream ends.
pub fn response<S>(stream: S) -> Response
where
    S: Stream<Item = String> + Send + 'static,
{
    // Boxing is what makes the wrapper `Unpin`; handlers hand over async streams
    // that cannot be pinned in place themselves.
    let boxed: futures_util::stream::BoxStream<'static, String> = Box::pin(stream);
    let body = Body::from_stream(WithHeartbeat::new(boxed));
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream; charset=utf-8"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache, no-transform"),
    );
    headers.insert(header::CONNECTION, HeaderValue::from_static("keep-alive"));
    headers.insert(
        header::HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    (StatusCode::OK, headers, body).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journaled_frames_carry_the_cursor_and_scoped_ones_do_not() {
        let data = serde_json::json!({ "delta": "hi" });
        assert_eq!(
            frame(TEXT, &data, Some(7)),
            "id: 7\nevent: text\ndata: {\"delta\":\"hi\"}\n\n"
        );
        assert!(!frame(CONNECTED, &data, None).starts_with("id:"));
    }
}
