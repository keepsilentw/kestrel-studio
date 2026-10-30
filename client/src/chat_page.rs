//! The conversation page's DOM half: subscribe, paint, submit.
//!
//! Every decision — what to paint, whether a message was already painted, whether
//! a cursor means "reload" — lives in [`crate::chat_core`] and is asserted there.
//! This file only turns [`Paint`] values into elements, and it copies the class
//! names and `data-*` attributes from `web/scripts/main.ts` and the hbs partials,
//! because the stylesheet and the SSR shell key on them: a reloaded page has to
//! look like the live one, and that claim is exactly what this rewrite can break.
//!
//! Borrow rule, same as `voice_page`: **no `RefCell` borrow is held across a call
//! that can re-enter.** Every handler clones the element handles it needs out of
//! the borrow first, then borrows again only for the state it mutates.
//!
//! Scope of this slice: stream subscription, turn painting, mode selector,
//! composer, image preview. **Not here yet:** the download dialog (canvas
//! re-encode), the delete-confirmation dialog, the right sidebar's turn index and
//! the history dropdown refresh. `/` therefore still loads `/assets/main.js`; this
//! module compiles and is not yet referenced by any template.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::{
    Document, Element, EventSource, Headers, HtmlButtonElement, HtmlDialogElement, HtmlElement,
    HtmlTextAreaElement, MessageEvent, Request, RequestInit, Response,
};

use crate::chat_core::{Paint, Stream};
use kestrel_proto::event::{ConversationEvent, MessageView, Mode};

/// The mode the selector starts in is remembered across visits.
pub const MODE_STORAGE_KEY: &str = "kestrel-studio:mode";

/// The journaled names. Listed here so an event the server adds and this page does
/// not handle is a visible gap in one place, not a silently unpainted turn.
pub const EVENT_NAMES: [&str; 11] = [
    "user_message",
    "turn_started",
    "reasoning",
    "text",
    "tool_call",
    "tool_result",
    "asset",
    "turn_done",
    "turn_error",
    "task_updated",
    "message_added",
];

/// The parts of one assistant turn the stream writes into.
struct Live {
    reasoning: HtmlElement,
    /// Reasoning arrives as deltas and is shown as plain text, so it is
    /// accumulated here instead of pushed into the DOM one chunk at a time.
    reasoning_text: String,
    content: HtmlElement,
    tools: HtmlElement,
    assets: HtmlElement,
    /// The assistant's text as it has arrived. Re-rendered whole on every delta:
    /// a markdown span can straddle two of them, so appending rendered HTML would
    /// split it.
    answer: String,
}

/// Element handles. Cheap to clone: they are JS object references.
#[derive(Clone)]
struct Dom {
    stream: HtmlElement,
    prompt: HtmlTextAreaElement,
    send: HtmlButtonElement,
    new_conversation: HtmlButtonElement,
    modes: Vec<HtmlButtonElement>,
    preview: HtmlDialogElement,
    preview_image: HtmlElement,
    body: HtmlElement,
}

struct Page {
    dom: Dom,
    journal: Stream,
    live: HashMap<i64, Live>,
    /// Pending-task chips by task id: `task_updated` arrives for a job that may
    /// have been submitted before this page was ever loaded.
    tasks: HashMap<i64, HtmlElement>,
    events: Option<EventSource>,
    mode: Mode,
    /// A turn is running for this conversation, so the composer waits: the server
    /// refuses a second concurrent turn with 409 on purpose.
    turn_running: bool,
    submitting: bool,
}

type Handle = Rc<RefCell<Page>>;

#[wasm_bindgen(js_name = initChat)]
pub fn init_chat() -> Result<(), JsValue> {
    let window = web_sys::window().ok_or("no window")?;
    let document = window.document().ok_or("no document")?;
    let body = document.body().ok_or("no body")?;

    let dom = Dom {
        stream: element(&document, "stream")?,
        prompt: element(&document, "prompt")?
            .dyn_into::<HtmlTextAreaElement>()
            .map_err(|_| JsValue::from_str("#prompt is not a textarea"))?,
        send: button(&document, "send")?,
        new_conversation: button(&document, "new-conversation")?,
        modes: mode_buttons(&document)?,
        preview: dialog(&document, "preview")?,
        preview_image: element(&document, "preview-image")?,
        body,
    };

    let mode = restore_mode(&dom);
    let page: Handle = Rc::new(RefCell::new(Page {
        dom: dom.clone(),
        journal: Stream::new(),
        live: HashMap::new(),
        tasks: HashMap::new(),
        events: None,
        mode,
        turn_running: false,
        submitting: false,
    }));

    seed_rendered(&document, &page);
    paint_modes(&dom, mode);
    sync_composer(&page);
    bind(&page, &dom)?;

    // An existing conversation is subscribed immediately: a turn still running on
    // the server must reach this page even if nobody presses send here.
    if let Some(id) = conversation_id(&dom.body) {
        let cursor = dom.body.dataset().get("start-seq").unwrap_or_default();
        open_stream(&page, &id, &cursor);
    }
    Ok(())
}

fn element(document: &Document, id: &str) -> Result<HtmlElement, JsValue> {
    document
        .get_element_by_id(id)
        .ok_or_else(|| JsValue::from_str(&format!("missing #{id}")))?
        .dyn_into::<HtmlElement>()
        .map_err(|_| JsValue::from_str(&format!("#{id} is not an element")))
}

fn button(document: &Document, id: &str) -> Result<HtmlButtonElement, JsValue> {
    element(document, id)?
        .dyn_into::<HtmlButtonElement>()
        .map_err(|_| JsValue::from_str(&format!("#{id} is not a button")))
}

fn dialog(document: &Document, id: &str) -> Result<HtmlDialogElement, JsValue> {
    element(document, id)?
        .dyn_into::<HtmlDialogElement>()
        .map_err(|_| JsValue::from_str(&format!("#{id} is not a dialog")))
}

fn mode_buttons(document: &Document) -> Result<Vec<HtmlButtonElement>, JsValue> {
    let found = document.query_selector_all("[data-mode]")?;
    let mut buttons = Vec::new();
    for index in 0..found.length() {
        if let Some(node) = found.item(index) {
            buttons.push(node.dyn_into::<HtmlButtonElement>()?);
        }
    }
    Ok(buttons)
}

fn conversation_id(body: &HtmlElement) -> Option<String> {
    body.dataset().get("conversation-id").filter(|id| !id.is_empty())
}

/// The ids the SSR shell already painted. Without this the replay from
/// `data-start-seq` would show every stored turn a second time.
fn seed_rendered(document: &Document, page: &Handle) {
    let Ok(nodes) = document.query_selector_all("[data-message-id]") else {
        web_sys::console::error_1(&JsValue::from_str(
            "kestrel: no [data-message-id] nodes; the replay filter cannot work",
        ));
        return;
    };
    let mut page = page.borrow_mut();
    for index in 0..nodes.length() {
        let Some(node) = nodes.item(index) else { continue };
        let Some(html) = node.dyn_ref::<HtmlElement>() else { continue };
        if let Some(raw) = html.dataset().get("message-id").and_then(|value| value.parse::<i64>().ok()) {
            page.journal.note_rendered(raw);
        }
    }
}

/// The shell's `[data-mode]` is what the server rendered; storage only overrides
/// it when it names a mode this build knows, so the buttons cannot disagree with
/// the page.
fn restore_mode(dom: &Dom) -> Mode {
    let shell = dom.body.dataset().get("mode").and_then(|raw| Mode::from_storage(Some(&raw)));
    let stored = web_sys::window()
        .and_then(|window| window.local_storage().ok().flatten())
        .and_then(|store| store.get_item(MODE_STORAGE_KEY).ok().flatten())
        .and_then(|raw| crate::chat_core::stored_mode(Some(&raw)));
    stored.or(shell).unwrap_or(Mode::Auto)
}

fn paint_modes(dom: &Dom, active: Mode) {
    for button in &dom.modes {
        let name = button.dataset().get("mode").unwrap_or_default();
        let on = name == active.as_str();
        button.set_class_name(if on { "mode-button mode-button-active" } else { "mode-button" });
    }
}

/// The composer is the only place a user can spend money, so its state is derived,
/// never guessed: disabled while submitting or while a turn runs.
fn sync_composer(page: &Handle) {
    let (dom, busy) = {
        let borrow = page.borrow();
        (borrow.dom.clone(), borrow.submitting || borrow.turn_running)
    };
    dom.send.set_disabled(busy);
    dom.prompt.set_attribute("placeholder", if busy { "正在生成…" } else { "想让红隼帮你做什么？" }).ok();
}

fn bind(page: &Handle, dom: &Dom) -> Result<(), JsValue> {
    for button in dom.modes.clone() {
        let scoped = Rc::clone(page);
        let Some(name) = button.dataset().get("mode") else { continue };
        let closure = Closure::<dyn FnMut()>::new(move || {
            let Some(mode) = Mode::from_storage(Some(&name)) else { return };
            scoped.borrow_mut().mode = mode;
            if let Some(store) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) {
                let _ = store.set_item(MODE_STORAGE_KEY, mode.as_str());
            }
            let current = scoped.borrow().dom.clone();
            paint_modes(&current, mode);
        });
        button.add_event_listener_with_callback("click", closure.as_ref().unchecked_ref())?;
        closure.forget();
    }

    // The form's own submit would navigate the page; a turn goes out over fetch and
    // comes back on the stream, so both the submit button and Enter are wired.
    let scoped = Rc::clone(page);
    let submit = Closure::<dyn FnMut(web_sys::Event)>::new(move |event: web_sys::Event| {
        event.prevent_default();
        send_prompt(&scoped);
    });
    dom.send
        .add_event_listener_with_callback("click", submit.as_ref().unchecked_ref())?;
    submit.forget();

    let scoped = Rc::clone(page);
    let fresh = Closure::<dyn FnMut()>::new(move || {
        // A new conversation is a page reset, not a client-side clear: the shell
        // decides what an empty conversation looks like.
        if let Some(window) = web_sys::window() {
            let _ = window.location().set_search("");
            window.location().reload().ok();
        }
        let _ = scoped;
    });
    dom.new_conversation
        .add_event_listener_with_callback("click", fresh.as_ref().unchecked_ref())?;
    fresh.forget();

    // One delegated listener for previews: assets are appended continuously, and
    // a listener per image would be a leak the length of the conversation.
    let scoped = Rc::clone(page);
    let click = Closure::<dyn FnMut(web_sys::MouseEvent)>::new(move |event: web_sys::MouseEvent| {
        let Some(target) = event
            .target()
            .and_then(|value| value.dyn_into::<Element>().ok())
        else {
            return;
        };
        if !target.class_list().contains("asset-image") {
            return;
        }
        let Some(src) = target.get_attribute("src") else { return };
        let (preview, preview_image) = {
            let borrow = scoped.borrow();
            (borrow.dom.preview.clone(), borrow.dom.preview_image.clone())
        };
        preview_image.set_attribute("src", &src).ok();
        preview.set_attribute("data-src", &src).ok();
        if !preview.open() {
            // A dialog that refuses to open is the feature failing, not a detail:
            // say which. `showModal` throws when the element is not a dialog or the
            // document is not active.
            if let Err(error) = preview.show_modal() {
                web_sys::console::error_1(&JsValue::from_str(&format!(
                    "kestrel: cannot open the preview: {error:?}"
                )));
            }
        }
    });
    dom.stream
        .add_event_listener_with_callback("click", click.as_ref().unchecked_ref())?;
    click.forget();

    Ok(())
}

/// Subscribe to the conversation's event stream. `from` is the journal cursor the
/// shell stamped; empty means "from the beginning" for a brand-new conversation.
fn open_stream(page: &Handle, id: &str, from: &str) {
    close_stream(page);
    let path = if from.is_empty() {
        format!("/api/v1/conversations/{id}/stream")
    } else {
        format!("/api/v1/conversations/{id}/stream?from={}", js_sys::encode_uri_component(from))
    };
    let Ok(source) = EventSource::new(&path) else {
        web_sys::console::error_1(&JsValue::from_str(&format!("kestrel: cannot subscribe {path}")));
        return;
    };

    let scoped = Rc::clone(page);
    let resync = Closure::<dyn FnMut()>::new(move || {
        // The cursor is behind the retained window: the missing events cannot be
        // rebuilt here, so the only honest move is a fresh shell.
        if let Some(window) = web_sys::window() {
            window.location().reload().ok();
        }
        let _ = &scoped;
    });
    source.add_event_listener_with_callback(Stream::RESYNC_EVENT, resync.as_ref().unchecked_ref()).ok();
    resync.forget();

    for name in EVENT_NAMES {
        let scoped = Rc::clone(page);
        let listener = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
            let Some(raw) = event.data().as_string() else {
                web_sys::console::error_1(&JsValue::from_str(&format!(
                    "kestrel: a {name} frame is not text"
                )));
                return;
            };
            let Ok(parsed) = serde_json::from_str::<ConversationEvent>(&raw) else {
                // Loud: this is the shape the page no longer restates by hand, so a
                // failure here means server and client disagree about the wire.
                web_sys::console::error_1(&JsValue::from_str(&format!(
                    "kestrel: cannot parse the {name} frame"
                )));
                return;
            };
            let paints = { scoped.borrow_mut().journal.on_event(&parsed) };
            for paint in paints {
                apply(&scoped, paint);
            }
        });
        source.add_event_listener_with_callback(name, listener.as_ref().unchecked_ref()).ok();
        listener.forget();
    }

    page.borrow_mut().events = Some(source);
}

fn close_stream(page: &Handle) {
    if let Some(source) = page.borrow_mut().events.take() {
        source.close();
    }
}

/// Paint one instruction. Borrows are taken per statement so nothing here can
/// re-enter itself through a DOM call.
fn apply(page: &Handle, paint: Paint) {
    let dom = page.borrow().dom.clone();
    match paint {
        Paint::Reload => {
            if let Some(window) = web_sys::window() {
                window.location().reload().ok();
            }
        }
        Paint::RenderMessage { message } => {
            let Some(document) = dom.stream.owner_document() else {
                web_sys::console::error_1(&JsValue::from_str("kestrel: the stream has no owner document"));
                return;
            };
            let article = render_message(&document, &message);
            dom.stream.append_child(&article).ok();
            scroll_bottom(&dom.stream);
        }
        Paint::OpenTurn { message_id, mode } => {
            let Some(document) = dom.stream.owner_document() else { return };
            let article = open_turn(&document, mode);
            let live = Live {
                reasoning: child(&article, "reasoning-body"),
                reasoning_text: String::new(),
                content: child(&article, "content"),
                tools: child(&article, "tool-calls"),
                assets: child(&article, "assets"),
                answer: String::new(),
            };
            dom.stream.append_child(&article).ok();
            page.borrow_mut().live.insert(message_id, live);
            page.borrow_mut().turn_running = true;
            sync_composer(page);
            scroll_bottom(&dom.stream);
        }
        Paint::Reasoning { message_id, delta } => {
            let text = {
                let mut borrow = page.borrow_mut();
                let Some(live) = borrow.live.get_mut(&message_id) else { return };
                live.reasoning_text.push_str(&delta);
                live.reasoning_text.clone()
            };
            // Written wholesale rather than appended per chunk, because the block is
            // a `pre` of plain text and the deltas are text fragments.
            if let Some(live) = page.borrow().live.get(&message_id) {
                live.reasoning.set_text_content(Some(&text));
            }
        }
        Paint::Text { message_id, delta } => {
            let mut borrow = page.borrow_mut();
            if let Some(live) = borrow.live.get_mut(&message_id) {
                live.answer.push_str(&delta);
                // Re-rendered whole: a markdown span can straddle two deltas, and
                // appending rendered HTML per delta would cut it in half.
                live.content.set_inner_html(&kestrel_md::markdown(&live.answer));
            }
            drop(borrow);
            scroll_bottom(&dom.stream);
        }
        Paint::ToolCall { message_id, name, status } => {
            let label = if status == "running" { "执行中…" } else { "已完成" };
            let text = format!("{name} {label}");
            if let Some(tools) = page.borrow().live.get(&message_id).map(|live| live.tools.clone()) {
                append_item(&tools, &text, "tool-call");
            }
        }
        Paint::ToolResult { message_id, name, ok, summary } => {
            let text = format!("{name} — {summary}");
            let class = if ok { "tool-call tool-call-ok" } else { "tool-call tool-call-fail" };
            if let Some(tools) = page.borrow().live.get(&message_id).map(|live| live.tools.clone()) {
                append_item(&tools, &text, class);
            }
        }
        Paint::Asset { message_id, kind, url } => {
            let container = page.borrow().live.get(&message_id).map(|live| live.assets.clone());
            if let (Some(document), Some(container)) = (dom.stream.owner_document(), container) {
                append_asset(&document, &container, &kind, &url);
                scroll_bottom(&dom.stream);
            }
        }
        Paint::TurnDone { message_id } => {
            page.borrow_mut().live.remove(&message_id);
            page.borrow_mut().turn_running = false;
            sync_composer(page);
        }
        Paint::TurnError { message_id, message } => {
            {
                let borrow = page.borrow_mut();
                if let Some(live) = borrow.live.get(&message_id) {
                    live.content.set_text_content(Some(&message));
                    live.content.set_class_name("content turn-error");
                }
            }
            page.borrow_mut().live.remove(&message_id);
            page.borrow_mut().turn_running = false;
            sync_composer(page);
        }
        Paint::TaskUpdated { task_id, kind, status, error } => {
            task_chip(page, &dom, task_id, &kind, &status, error.as_deref());
        }
    }
}

fn child(article: &HtmlElement, class: &str) -> HtmlElement {
    article
        .query_selector(&format!(".{class}"))
        .ok()
        .flatten()
        .and_then(|node| node.dyn_into::<HtmlElement>().ok())
        .unwrap_or_else(|| {
            // A missing part means the shell's markup and this file disagree. Say
            // so instead of dropping the turn into a detached node.
            web_sys::console::error_1(&JsValue::from_str(&format!("kestrel: no .{class} in the turn")) );
            article.clone()
        })
}

/// The assistant shell, in the shapes `partials/turn.hbs` already styles.
/// Build one element with its class already set.
///
/// `create_element` hands back a plain `Element`, and every node this page paints
/// is an HTML element, so the cast is folded here once instead of at each call.
/// The two `expect`s are unreachable by construction: the tags are literals in
/// this file, and `dyn_into` only fails if one of them stops being an `HTMLElement`.
fn build(document: &Document, tag: &str, class: &str) -> HtmlElement {
    let element = document.create_element(tag).expect("a literal tag name");
    let html: HtmlElement = element.dyn_into().expect("a literal HTML tag");
    html.set_class_name(class);
    html
}

fn open_turn(document: &Document, mode: Option<Mode>) -> HtmlElement {
    let article = build(document, "article", "turn turn-assistant");

    if let Some(mode) = mode {
        let badge = build(document, "span", "turn-mode");
        badge.set_text_content(Some(mode.label()));
        article.append_child(&badge).ok();
    }

    let details = build(document, "details", "reasoning");
    let summary = build(document, "summary", "");
    summary.set_text_content(Some("思考过程"));
    let body = build(document, "pre", "reasoning-body");
    details.append_child(&summary).ok();
    details.append_child(&body).ok();

    let tools = build(document, "ul", "tool-calls");
    let content = build(document, "div", "content");
    let assets = build(document, "div", "assets");

    for node in [&details, &tools, &content, &assets] {
        article.append_child(node).ok();
    }
    article
}

fn render_message(document: &Document, message: &MessageView) -> HtmlElement {
    let article = build(document, "article", &format!("turn turn-{}", message.role));
    article.set_attribute("data-message-id", &message.id.to_string()).ok();

    if let Some(mode) = message.mode {
        let badge = build(document, "span", "turn-mode");
        badge.set_text_content(Some(mode.label()));
        article.append_child(&badge).ok();
    }

    if message.role == "user" {
        // User turns are plain text: markdown on the user's own words made a
        // reloaded turn differ from the live one (commit e7b2911).
        let content = build(document, "div", "content");
        content.set_text_content(Some(&message.content));
        article.append_child(&content).ok();
        return article;
    }

    if let Some(reasoning) = &message.reasoning {
        if !reasoning.is_empty() {
            let details = build(document, "details", "reasoning");
            let summary = build(document, "summary", "");
            summary.set_text_content(Some("思考过程"));
            let body = build(document, "pre", "reasoning-body");
            body.set_text_content(Some(reasoning));
            details.append_child(&summary).ok();
            details.append_child(&body).ok();
            article.append_child(&details).ok();
        }
    }

    if let Some(calls) = &message.tool_calls {
        if !calls.is_empty() {
            let list = build(document, "ul", "tool-calls");
            for call in calls {
                append_item(&list, &format!("{} — {}", call.name, call.summary), "tool-call");
            }
            article.append_child(&list).ok();
        }
    }

    if !message.content.is_empty() {
        let content = build(document, "div", "content");
        content.set_inner_html(&kestrel_md::markdown(&message.content));
        article.append_child(&content).ok();
    }

    if !message.assets.is_empty() {
        let list = build(document, "div", "assets");
        for asset in &message.assets {
            append_asset(document, &list, asset.kind.as_str(), &asset.url);
        }
        article.append_child(&list).ok();
    }
    article
}

fn append_item(list: &HtmlElement, text: &str, class: &str) {
    let Some(document) = list.owner_document() else { return };
    let item = build(&document, "li", class);
    item.set_text_content(Some(text));
    list.append_child(&item).ok();
}

fn append_asset(document: &Document, container: &HtmlElement, kind: &str, url: &str) {
    let figure = build(document, "figure", "asset");
    let video = kind == "video";
    let media = build(
        document,
        if video { "video" } else { "img" },
        if video { "asset-video" } else { "asset-image" },
    );
    media.set_attribute("src", url).ok();
    if video {
        media.set_attribute("controls", "").ok();
        media.set_attribute("preload", "metadata").ok();
    } else {
        media.set_attribute("alt", "生成的图片").ok();
    }
    let caption = build(document, "figcaption", "asset-actions");
    let link = build(document, "a", "asset-download");
    link.set_attribute("href", url).ok();
    link.set_attribute("download", "").ok();
    link.set_text_content(Some("下载"));
    caption.append_child(&link).ok();
    figure.append_child(&media).ok();
    figure.append_child(&caption).ok();
    container.append_child(&figure).ok();
}

/// The pending-task chip: a video keeps rendering after its turn is over, and the
/// only signal it exists is this row updating from `task_updated`.
fn task_chip(page: &Handle, dom: &Dom, task_id: i64, kind: &str, status: &str, error: Option<&str>) {
    let Some(document) = dom.stream.owner_document() else { return };
    let label = match kestrel_proto::TaskStatus::parse(status) {
        kestrel_proto::TaskStatus::Failed => {
            error.map(|text| format!("{kind} 失败：{text}")).unwrap_or_else(|| format!("{kind} 失败"))
        }
        other => format!("{kind} {}", other.label()),
    };

    let existing = page.borrow().tasks.get(&task_id).cloned();
    if let Some(chip) = existing {
        chip.set_text_content(Some(&label));
        return;
    }
    let chip = build(&document, "div", "pending-task");
    chip.set_attribute("data-task-id", &task_id.to_string()).ok();
    chip.set_text_content(Some(&label));
    dom.stream.append_child(&chip).ok();
    page.borrow_mut().tasks.insert(task_id, chip);
    scroll_bottom(&dom.stream);
}

fn scroll_bottom(stream: &HtmlElement) {
    stream.set_scroll_top(stream.scroll_height());
}

/// POST one turn. Nothing here paints: the echo, the shell and every delta come
/// back on the stream in journal order, which is the whole point of splitting
/// submit from subscribe (docs/architecture.md §4.2).
fn send_prompt(page: &Handle) {
    let (dom, mode) = {
        let borrow = page.borrow();
        (borrow.dom.clone(), borrow.mode)
    };
    let prompt = dom.prompt.value();
    let prompt = prompt.trim().to_string();
    if prompt.is_empty() {
        return;
    }
    {
        let mut borrow = page.borrow_mut();
        borrow.submitting = true;
    }
    sync_composer(page);

    let scoped = Rc::clone(page);
    spawn_local(async move {
        let outcome = run_turn(&dom, &scoped, &prompt, mode).await;
        if let Err(message) = outcome {
            web_sys::console::error_1(&JsValue::from_str(&format!("kestrel: {message}")));
            append_item(&dom.stream, &message, "turn-error");
        } else {
            dom.prompt.set_value("");
        }
        scoped.borrow_mut().submitting = false;
        sync_composer(&scoped);
    });
}

async fn run_turn(dom: &Dom, page: &Handle, prompt: &str, mode: Mode) -> Result<(), String> {
    let mut id = conversation_id(&dom.body).unwrap_or_default();
    if id.is_empty() {
        id = create_conversation(dom, page, prompt).await?;
    }
    let status = post_json(&format!("/api/v1/conversations/{id}/turns"), &serde_json::json!({
        "prompt": prompt,
        "mode": mode.as_str(),
    }))
    .await?;

    if status == 404 {
        // The conversation went away since the shell was painted (another tab
        // deleted it). The prompt was never stored, so recreating and retrying
        // once is safe; a second failure is reported rather than looped on.
        let id = create_conversation(dom, page, prompt).await?;
        let retried = post_json(&format!("/api/v1/conversations/{id}/turns"), &serde_json::json!({
            "prompt": prompt,
            "mode": mode.as_str(),
        }))
        .await?;
        if retried != 202 {
            return Err(format!("提交轮次失败（HTTP {retried}）"));
        }
        return Ok(());
    }
    if status != 202 {
        return Err(format!("提交轮次失败（HTTP {status}）"));
    }
    Ok(())
}

async fn create_conversation(dom: &Dom, page: &Handle, prompt: &str) -> Result<String, String> {
    let value = post_json_value("/api/v1/conversations", &serde_json::json!({ "prompt": prompt }))
        .await?
        .ok_or_else(|| "创建会话的响应没有正文".to_string())?;
    let id = value
        .get("conversationId")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| "创建会话的响应没有 conversationId".to_string())?;
    let id = id.to_string();

    dom.body.set_attribute("data-conversation-id", &id).ok();
    // Keep the address bar honest so a reload restores this conversation.
    if let Some(window) = web_sys::window() {
        let path = window.location().pathname().unwrap_or_default();
        let url = format!("{path}?c={id}");
        if let Ok(history) = window.history() {
            let _ = history.replace_state_with_url(&JsValue::NULL, "", Some(&url));
        }
    }
    // The page is now subscribed to a conversation the shell never knew about.
    open_stream(&Rc::clone(page), &id, "0");
    Ok(id)
}

/// POST with a JSON body and return the status code.
async fn post_json(path: &str, body: &serde_json::Value) -> Result<u16, String> {
    let response = post_send(path, body).await?;
    Ok(response.status())
}

/// The same request, with the parsed body when the endpoint returns one.
async fn post_json_value(path: &str, body: &serde_json::Value) -> Result<Option<serde_json::Value>, String> {
    let response = post_send(path, body).await?;
    let status = response.status();
    if !(200..300).contains(&status) {
        return Err(format!("{path} 失败（HTTP {status}）"));
    }
    if status == 202 {
        return Ok(None);
    }
    let text = response.text().map_err(|_| "no body".to_string())?;
    let value = JsFuture::from(text).await.map_err(|_| "unreadable body".to_string())?;
    let Some(raw) = value.as_string() else { return Ok(None) };
    Ok(Some(serde_json::from_str(&raw).map_err(|_| "body is not JSON")?))
}

async fn post_send(path: &str, body: &serde_json::Value) -> Result<Response, String> {
    let window = web_sys::window().ok_or("no window")?;
    let headers = Headers::new().map_err(|_| "cannot build headers")?;
    headers
        .append("content-type", "application/json")
        .map_err(|_| "cannot set the content type")?;
    let init = RequestInit::new();
    init.set_method("POST");
    init.set_headers(&JsValue::from(headers));
    init.set_body(&JsValue::from_str(&body.to_string()));
    let request = Request::new_with_str_and_init(path, &init).map_err(|_| "cannot build the request")?;

    let value = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|_| format!("{path} 请求失败"))?;
    value.dyn_into::<Response>().map_err(|_| "malformed response".to_string())
}
