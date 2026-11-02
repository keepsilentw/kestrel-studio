//! The `/voice` page: elements, the hold, and what the browser does with each
//! effect.
//!
//! This is the only module that touches the DOM, and it is deliberately the
//! thinnest one. Every decision — which frame goes when, what a release before
//! `ready` means, when the 25 s cap fires — is in [`crate::voice_core`] and is
//! asserted there without a browser. If a behaviour looks like it belongs here,
//! it probably belongs in the core instead.
//!
//! Borrow rule, because this is where a WASM client panics: **no `RefCell`
//! borrow is held across a call that can re-enter the page.** Browser callbacks
//! (pointer events, socket frames, worklet messages, the interval) all take the
//! borrow themselves and release it before doing anything that could dispatch
//! another one, which is why [`apply`] borrows per effect and re-borrows inside
//! the loop instead of once around it.

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{Element, EventSource, HtmlButtonElement, HtmlElement, MessageEvent, Window};

use crate::audio::{self, AudioGraph};
use crate::pcm;
use crate::voice_core::{Effect, Session, TICK_MS};
use crate::wire::{self, SocketSlot, WireEvent};

/// The elements the page needs. The ids are written once, here, and
/// `rust/src/view/tests.rs` reads them out of this file's source and asserts the
/// server's voice template carries each one — the same mechanism that guards the
/// chat page's contract against `web/scripts/main.ts`, now between two Rust files.
struct Dom {
    log: HtmlElement,
    status: HtmlElement,
    button: HtmlButtonElement,
}

struct Page {
    session: Session<SocketSlot>,
    dom: Dom,
    events: Option<EventSource>,
    /// A ticket request is in flight, so no socket exists yet. This is the state
    /// the Nest client modelled as `readyState === CONNECTING`, and it is what a
    /// fast second press would otherwise miss: dialing twice leaves the first
    /// upstream session running on the server with nobody listening to it.
    dialing: bool,
}

struct Shared {
    page: RefCell<Page>,
    /// Its own `Rc` because the microphone opener is an async task that outlives
    /// the call that started it, and it must not hold the page alive to do so.
    audio: Rc<RefCell<AudioGraph>>,
}

type Handle = Rc<Shared>;

/// Entry point, called from the page's module script once the audio slice exists.
#[wasm_bindgen(js_name = initVoice)]
pub fn init_voice() -> Result<(), JsValue> {
    let window = web_sys::window().ok_or("no window")?;
    let document = window.document().ok_or("no document")?;

    let dom = Dom {
        log: element(&document, "voice-log")?,
        status: element(&document, "voice-status")?,
        // One lookup form — `element(&document, <id>)` — so the contract test in
        // `rust/src/view/tests.rs` can read the ids out of this file without
        // having to guess which call style the page happened to use.
        button: element(&document, "voice-button")?
            .dyn_into::<HtmlButtonElement>()
            .map_err(|_| "#voice-button is not a button")?,
    };

    let shared: Handle = Rc::new(Shared {
        page: RefCell::new(Page {
            session: Session::new(SocketSlot::default()),
            dom,
            events: None,
            dialing: false,
        }),
        audio: Rc::new(RefCell::new(AudioGraph::new())),
    });

    // No microphone API (an insecure origin hides `mediaDevices` entirely) is
    // stated on the page instead of failing on the first press.
    if !crate::wasm::has_microphone() {
        let shared = Rc::clone(&shared);
        apply(
            &shared,
            vec![
                Effect::SetButtonEnabled(false),
                Effect::AppendEntry { kind: "error", text: "拿不到麦克风权限，无法说话".to_string() },
            ],
        );
        return Ok(());
    }

    bind(&shared, &window)?;
    Ok(())
}

fn element(document: &web_sys::Document, id: &str) -> Result<HtmlElement, JsValue> {
    document
        .get_element_by_id(id)
        .ok_or_else(|| JsValue::from_str(&format!("missing #{id}")))?
        .dyn_into::<HtmlElement>()
        .map_err(|_| JsValue::from_str(&format!("#{id} is not an element")))
}

fn bind(shared: &Handle, window: &Window) -> Result<(), JsValue> {
    let button = { shared.page.borrow().dom.button.clone() };

    let scoped = Rc::clone(shared);
    let press = Closure::wrap(Box::new(move |event: web_sys::Event| {
        // Without this the long press also selects text and opens the context
        // menu on touch devices, which reads as the button not working.
        event.prevent_default();
        let effects = scoped.page.borrow_mut().session.press(now_ms());
        apply(&scoped, effects);
    }) as Box<dyn Fn(web_sys::Event)>);
    button.add_event_listener_with_callback("pointerdown", press.as_ref().unchecked_ref())?;
    press.forget();

    let scoped = Rc::clone(shared);
    let release = Closure::wrap(Box::new(move || {
        let effects = scoped.page.borrow_mut().session.release();
        apply(&scoped, effects);
    }) as Box<dyn Fn()>);
    for event in ["pointerup", "pointercancel", "pointerleave"] {
        button.add_event_listener_with_callback(event, release.as_ref().unchecked_ref())?;
    }
    release.forget();

    // The cap is a deadline the core holds; this is only the clock that reads it.
    let scoped = Rc::clone(shared);
    let tick = Closure::wrap(Box::new(move || {
        let effects = scoped.page.borrow_mut().session.tick(now_ms());
        apply(&scoped, effects);
    }) as Box<dyn Fn()>);
    window.set_interval_with_callback_and_timeout_and_arguments_0(
        tick.as_ref().unchecked_ref(),
        TICK_MS,
    )?;
    tick.forget();

    let scoped = Rc::clone(shared);
    let leaving = Closure::wrap(Box::new(move || {
        let effects = {
            let mut page = scoped.page.borrow_mut();
            if let Some(source) = page.events.take() {
                source.close();
            }
            page.session.shutdown()
        };
        // The microphone goes first: leaving a live track behind keeps the
        // browser's recording indicator on after the page is gone.
        scoped.audio.borrow_mut().stop_capture();
        apply(&scoped, effects);
    }) as Box<dyn Fn()>);
    window.add_event_listener_with_callback("pagehide", leaving.as_ref().unchecked_ref())?;
    leaving.forget();

    Ok(())
}

fn now_ms() -> u64 {
    js_sys::Date::now() as u64
}

/// Paint the effects the core produced. Borrowed per effect on purpose: handling
/// one can dispatch something that re-enters (opening the socket arms a future
/// that may already be delivering a frame by the time this loop continues).
fn apply(shared: &Handle, effects: Vec<Effect>) {
    for effect in effects {
        match effect {
            Effect::OpenSocket => open_socket(shared),
            Effect::StartCapture => start_capture(shared),
            Effect::SetStatus(text) => shared.page.borrow().dom.status.set_text_content(Some(text)),
            Effect::AppendEntry { kind, text } => append_entry(shared, kind, &text),
            Effect::ShowAsset { kind, url } => append_asset(shared, &kind, &url),
            Effect::SetButtonEnabled(enabled) => {
                shared.page.borrow().dom.button.set_disabled(!enabled);
            }
            Effect::SetButtonActive(active) => {
                let button = shared.page.borrow().dom.button.clone();
                let classes = button.class_list();
                let outcome = if active {
                    classes.add_1("voice-button-active")
                } else {
                    classes.remove_1("voice-button-active")
                };
                if let Err(error) = outcome {
                    wire::report(&format!("cannot restyle the button: {error:?}"));
                }
            }
            Effect::ConversationStream(id) => conversation_stream(shared, id),
            Effect::PlayAudio { pcm: encoded } => play_audio(shared, &encoded),
        }
    }
}

fn open_socket(shared: &Handle) {
    let slot = {
        let mut page = shared.page.borrow_mut();
        if page.dialing {
            return;
        }
        page.dialing = true;
        page.session.transport().clone()
    };
    let scoped = Rc::clone(shared);
    let emit = Rc::new(move |event: WireEvent| match event {
        WireEvent::Opened => {
            scoped.page.borrow_mut().dialing = false;
            let effects = scoped.page.borrow_mut().session.on_socket_open();
            apply(&scoped, effects);
        }
        WireEvent::Frame(raw) => {
            let effects = scoped.page.borrow_mut().session.on_server_frame(&raw, now_ms());
            apply(&scoped, effects);
        }
        WireEvent::Closed => {
            scoped.page.borrow_mut().dialing = false;
            scoped.audio.borrow_mut().stop_capture();
            let effects = scoped.page.borrow_mut().session.on_socket_close();
            apply(&scoped, effects);
        }
        WireEvent::Failed(message) => {
            scoped.page.borrow_mut().dialing = false;
            let effects = scoped.page.borrow_mut().session.abort(&message);
            apply(&scoped, effects);
        }
    });
    wire::open(&slot, emit);
}

/// Open the microphone once, and hand every frame it produces to the core.
fn start_capture(shared: &Handle) {
    if shared.audio.borrow().is_capturing() {
        return;
    }
    let audio = Rc::clone(&shared.audio);
    let scoped = Rc::clone(shared);
    let on_frame = Rc::new(move |bytes: Vec<u8>| {
        let encoded = pcm::to_base64(&bytes);
        let effects = scoped.page.borrow_mut().session.audio_chunk(encoded);
        apply(&scoped, effects);
    });
    let scoped = Rc::clone(shared);
    let on_result = Rc::new(move |outcome: Result<(), String>| {
        let Err(message) = outcome else {
            // Success needs nothing here: the status and the cap are the core's
            // business, and it already painted them when the button went down.
            return;
        };
        // A button that records nothing looks exactly like a button waiting for
        // speech, so this is stated — and the hold is abandoned instead of
        // committing an empty buffer.
        let effects = scoped.page.borrow_mut().session.abort(&message);
        apply(&scoped, effects);
    });
    audio::spawn_open_mic(audio, on_frame, on_result);
}

fn play_audio(shared: &Handle, encoded: &str) {
    let bytes = match pcm::from_base64(encoded) {
        Some(bytes) => bytes,
        // A frame we cannot read is a protocol mismatch, not noise: say so rather
        // than playing a click.
        None => {
            wire::report("an audio frame is not valid base64");
            return;
        }
    };
    if let Err(message) = shared.audio.borrow_mut().play_pcm(&bytes) {
        wire::report(&message);
    }
}

/// The conversation stream: a long video render lands as a new message on it,
/// not on the socket (docs/architecture.md §11.4).
fn conversation_stream(shared: &Handle, id: Option<i64>) {
    let previous = shared.page.borrow_mut().events.take();
    if let Some(source) = previous {
        source.close();
    }
    let Some(id) = id else { return };

    let url = format!("/api/v1/conversations/{id}/stream");
    let source = match EventSource::new(&url) {
        Ok(source) => source,
        Err(error) => {
            wire::report(&format!("cannot subscribe {url}: {error:?}"));
            return;
        }
    };

    let scoped = Rc::clone(shared);
    let listener = Closure::wrap(Box::new(move |event: MessageEvent| {
        let Some(raw) = event.data().as_string() else { return };
        let Ok(payload) = serde_json::from_str::<serde_json::Value>(&raw) else { return };
        let Some(message) = payload.get("message") else { return };
        if let Some(text) = message.get("content").and_then(serde_json::Value::as_str) {
            if !text.is_empty() {
                append_entry(&scoped, "assistant", text);
            }
        }
        for asset in message.get("assets").and_then(serde_json::Value::as_array).into_iter().flatten() {
            let kind = asset.get("kind").and_then(serde_json::Value::as_str).unwrap_or_default();
            let url = asset.get("url").and_then(serde_json::Value::as_str).unwrap_or_default();
            if !kind.is_empty() && !url.is_empty() {
                append_asset(&scoped, kind, url);
            }
        }
    }) as Box<dyn Fn(MessageEvent)>);
    if let Err(error) =
        source.add_event_listener_with_callback("message_added", listener.as_ref().unchecked_ref())
    {
        wire::report(&format!("cannot listen on the conversation stream: {error:?}"));
        return;
    }
    listener.forget();
    shared.page.borrow_mut().events = Some(source);
}

fn append_entry(shared: &Handle, kind: &str, text: &str) {
    let (document, log) = {
        let page = shared.page.borrow();
        (
            web_sys::window().and_then(|window| window.document()),
            page.dom.log.clone(),
        )
    };
    let Some(document) = document else { return };
    let Ok(entry) = document.create_element("li") else { return };
    entry.set_class_name(&format!("voice-entry voice-entry-{kind}"));
    entry.set_text_content(Some(text));
    append_and_scroll(&log, &entry);
}

/// A generated image or video plus its download link, in the shapes the existing
/// stylesheet already targets (`asset`, `asset-image`, `asset-video`,
/// `asset-actions`, `asset-download`).
fn append_asset(shared: &Handle, kind: &str, url: &str) {
    let (document, log) = {
        let page = shared.page.borrow();
        (
            web_sys::window().and_then(|window| window.document()),
            page.dom.log.clone(),
        )
    };
    let Some(document) = document else { return };
    let Ok(entry) = document.create_element("li") else { return };
    entry.set_class_name("voice-entry voice-entry-assistant");
    let Ok(figure) = document.create_element("figure") else { return };
    figure.set_class_name("asset");

    let video = kind == "video";
    let tag = if video { "video" } else { "img" };
    let Ok(media) = document.create_element(tag) else { return };
    media.set_class_name(if video { "asset-video" } else { "asset-image" });
    if let Err(error) = media.set_attribute("src", url) {
        wire::report(&format!("cannot set the asset source: {error:?}"));
    }
    // Images keep their dimensions to the stylesheet's aspect rules; videos get
    // the native controls. `loading="lazy"` is on the SSR side, where the size is
    // known from the stored asset row — see the Chrome hint in roadmap.md §3.
    if video {
        let _ = media.set_attribute("controls", "");
        let _ = media.set_attribute("preload", "metadata");
    } else {
        let _ = media.set_attribute("alt", "生成的图片");
    }

    let Ok(caption) = document.create_element("figcaption") else { return };
    caption.set_class_name("asset-actions");
    let Ok(link) = document.create_element("a") else { return };
    link.set_class_name("asset-download");
    let _ = link.set_attribute("href", url);
    let _ = link.set_attribute("download", "");
    link.set_text_content(Some("下载"));
    let _ = caption.append_child(&link);
    let _ = figure.append_child(&media);
    let _ = figure.append_child(&caption);
    let _ = entry.append_child(&figure);
    append_and_scroll(&log, &entry);
}

fn append_and_scroll(log: &HtmlElement, entry: &Element) {
    if let Err(error) = log.append_child(entry) {
        wire::report(&format!("cannot append to the log: {error:?}"));
        return;
    }
    // A voice log that does not follow the conversation is a log nobody reads.
    log.set_scroll_top(log.scroll_height());
}
