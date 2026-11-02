//! The voice socket: ticket, upgrade, and the one frame that starts a call.
//!
//! Kept separate from the page because this is the layer that broke in
//! docs/verification.md §7: the socket opened, the server never heard from it,
//! and both ends stayed quiet. Everything here that can fail out loud, does
//! (`report`), including a frame dropped because the socket was not open — a
//! silent drop is the exact shape of that outage.

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::{MessageEvent, Request, RequestInit, Response, WebSocket};

use crate::voice_core::Transport;
use kestrel_proto::ClientFrame;

/// The socket lives in a slot because it is created by asynchronous code while
/// the core session that writes through it is created earlier, on the button
/// press.
#[derive(Clone, Default)]
pub struct SocketSlot(Rc<RefCell<Option<WebSocket>>>);

impl SocketSlot {
    fn with(&self, action: impl FnOnce(&WebSocket)) -> bool {
        if let Some(socket) = self.0.borrow().as_ref() {
            action(socket);
            return true;
        }
        false
    }

    /// Close the socket the way the page leaves: `stop` first, then the socket.
    pub fn shutdown(&self) {
        if let Some(socket) = self.0.borrow().as_ref() {
            let _ = socket.send_with_str(r#"{"type":"stop"}"#);
            // `close()` can throw when the socket is already CLOSING. The page is
            // leaving in that case, and an error dialog would be worse than the
            // race, so the failure is only logged.
            if let Err(error) = socket.close() {
                report(&format!("the voice socket was already closing: {error:?}"));
            }
        }
    }
}

impl Transport for SocketSlot {
    fn send(&mut self, frame: &ClientFrame) {
        let raw = match serde_json::to_string(frame) {
            Ok(serialised) => serialised,
            Err(error) => {
                report(&format!("cannot serialise {error}"));
                return;
            }
        };
        let sent = self.with(|socket| {
            if socket.ready_state() != WebSocket::OPEN {
                report(&format!("socket is not open, dropped {}", tag(frame)));
                return;
            }
            if let Err(error) = socket.send_with_str(&raw) {
                report(&format!("send failed for {}: {error:?}", tag(frame)));
            }
        });
        if !sent {
            report(&format!("no socket yet, dropped {}", tag(frame)));
        }
    }
}

fn tag(frame: &ClientFrame) -> &'static str {
    match frame {
        ClientFrame::Start => "start",
        ClientFrame::Audio { .. } => "audio",
        ClientFrame::Commit => "commit",
        ClientFrame::Stop => "stop",
    }
}

pub(crate) fn report(text: &str) {
    web_sys::console::error_1(&JsValue::from_str(&format!("kestrel voice: {text}")));
}

/// What the page needs to know about the socket. Each variant maps to one core
/// call; no decisions happen here.
pub enum WireEvent {
    Opened,
    Frame(String),
    Closed,
    Failed(String),
}

/// Trade the session cookie for a single-use ticket, then upgrade.
///
/// The cookie cannot authorise the handshake — an upgrade never reaches the
/// request pipeline that would read it — hence the ticket
/// (docs/architecture.md §11.2). The socket is published into `slot` *before* the
/// handlers are installed: `WebSocket::new` cannot have fired `open` yet, because
/// events are only dispatched once this frame returns to the event loop, and the
/// core's first `send` (that `start` frame) must find something to write on.
pub fn open(slot: &SocketSlot, emit: Rc<dyn Fn(WireEvent)>) {
    let slot = slot.clone();
    spawn_local(async move {
        let ticket = match fetch_ticket().await {
            Ok(ticket) => ticket,
            Err(message) => {
                emit(WireEvent::Failed(message));
                return;
            }
        };
        let url = socket_url(&ticket);
        let socket = match WebSocket::new(&url) {
            Ok(socket) => socket,
            Err(error) => {
                emit(WireEvent::Failed(format!("cannot open the voice socket: {error:?}")));
                return;
            }
        };
        // `binaryType` is left alone on purpose: it only governs *binary* frames,
        // and this protocol has none. A binary frame reaching here means the
        // server changed shape, and the handler below reports it instead of
        // silently treating it as an unknown message.
        *slot.0.borrow_mut() = Some(socket.clone());

        let scoped = Rc::clone(&emit);
        let on_open = Closure::wrap(Box::new(move || scoped(WireEvent::Opened)) as Box<dyn Fn()>);
        socket.set_onopen(Some(on_open.as_ref().unchecked_ref()));
        on_open.forget();

        let scoped = Rc::clone(&emit);
        let on_message = Closure::wrap(Box::new(move |event: MessageEvent| {
            match event.data().as_string() {
                Some(text) => scoped(WireEvent::Frame(text)),
                None => report("a non-text frame arrived on the voice socket"),
            }
        }) as Box<dyn Fn(MessageEvent)>);
        socket.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        on_message.forget();

        let scoped = Rc::clone(&emit);
        let on_close = Closure::wrap(Box::new(move |_event: web_sys::CloseEvent| {
            *slot.0.borrow_mut() = None;
            scoped(WireEvent::Closed);
        }) as Box<dyn Fn(web_sys::CloseEvent)>);
        socket.set_onclose(Some(on_close.as_ref().unchecked_ref()));
        on_close.forget();
    });
}

fn socket_url(ticket: &str) -> String {
    let window = web_sys::window().expect("no window");
    let location = window.location();
    let host = location.host().unwrap_or_default();
    let secure = location.protocol().map(|scheme| scheme == "https:").unwrap_or(false);
    let scheme = if secure { "wss" } else { "ws" };
    format!("{scheme}://{host}/api/voice?ticket={ticket}")
}

async fn fetch_ticket() -> Result<String, String> {
    let window = web_sys::window().ok_or("no window")?;
    let init = RequestInit::new();
    init.set_method("POST");
    // No `credentials`: fetch already defaults to `same-origin`, which is the
    // only thing that can carry the session cookie to this endpoint. Naming
    // `include` here would turn a same-origin call into a credentialed
    // cross-origin one if the app is ever mounted behind another host.
    let request = Request::new_with_str_and_init("/api/voice/ticket", &init)
        .map_err(|_| "cannot build the ticket request")?;

    let response_value = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|_| "the ticket request failed")?;
    let response: Response = response_value.dyn_into().map_err(|_| "malformed ticket response")?;
    if !response.ok() {
        return Err(format!("无法开始语音会话（HTTP {}）", response.status()));
    }

    let body = response.text().map_err(|_| "the ticket response has no body")?;
    let text_value = JsFuture::from(body)
        .await
        .map_err(|_| "no ticket body")?;
    let body = text_value.as_string().ok_or("ticket body is not text")?;
    let parsed: serde_json::Value = serde_json::from_str(&body).map_err(|_| "ticket body is not JSON")?;
    let ticket = urlencoding_ticket(&parsed);
    if ticket.is_empty() {
        return Err("服务器没有签发语音票据".to_string());
    }
    Ok(ticket)
}

/// The ticket goes into a query string, so it is percent-encoded here rather
/// than relying on whatever characters the server happened to produce.
fn urlencoding_ticket(parsed: &serde_json::Value) -> String {
    let raw = parsed.get("ticket").and_then(serde_json::Value::as_str).unwrap_or_default();
    js_sys::encode_uri_component(raw).into()
}

