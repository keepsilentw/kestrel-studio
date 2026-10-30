//! The browser-facing surface: what gets exported to JS, and what the page asks
//! the WASM module for.
//!
//! Two functions here, both things the pages actually need today and both
//! previously served by an npm package:
//!
//! * [`render_markdown`] replaces `marked`. It is also how the client stops
//!   drifting from the server: assistant text is rendered by one markdown engine
//!   on each side of the wire, from the same crate, with the same options — the
//!   old setup had `marked` in Node SSR, `marked` again in the browser, and a
//!   hand-written subset in `rust/src/view/html.rs`, and the shape of a turn
//!   differed depending on whether you watched it arrive or reloaded it
//!   (docs/implementation-notes.md §8, commit `e7b2911`).
//! * [`has_microphone`] answers the question `/voice` asks before wiring the
//!   button: `navigator.mediaDevices` is missing on insecure origins, and the
//!   page must say so instead of failing on the first press.
//!
//! The rest of the DOM layer (socket, audio, rendering) is still in
//! `web/scripts/*.ts`; it lands here in slices, each one compile-checked, and
//! only exercised at runtime on a remote runner or the target host — this
//! machine does not start services or Docker (CLAUDE.md).

use js_sys::Array;
use wasm_bindgen::prelude::*;

/// Options shared with the server-side renderer. Kept narrow on purpose: the
/// client used to get GFM tables and task lists from `marked`'s defaults, and
/// whether the server produced the same HTML was never asserted.
fn markdown_options() -> pulldown_cmark::Options {
    pulldown_cmark::Options::ENABLE_TABLES | pulldown_cmark::Options::ENABLE_STRIKETHROUGH
}

/// Render assistant text to HTML. The caller inserts the result as `innerHTML`,
/// so the markdown source is the only thing that can shape the output: no raw
/// HTML passthrough, no `unsafe` extension.
#[wasm_bindgen(js_name = renderMarkdown)]
pub fn render_markdown(source: &str) -> String {
    let parser = pulldown_cmark::Parser::new_ext(source, markdown_options());
    let mut html = String::with_capacity(source.len() + 32);
    pulldown_cmark::html::push_html(&mut html, parser);
    html
}

/// `navigator.mediaDevices` present, i.e. is push-to-talk possible at all on this
/// page. False on insecure origins, where the browser hides the API entirely.
#[wasm_bindgen(js_name = hasMicrophone)]
pub fn has_microphone() -> bool {
    let Some(window) = web_sys::window() else {
        return false;
    };
    window.navigator().media_devices().is_ok()
}

/// What the module was built with, for the console line that makes a stale asset
/// obvious during a canary deploy. `Array` because a JS string would need a
/// second hop to read.
#[wasm_bindgen(js_name = buildInfo)]
pub fn build_info() -> Array {
    let out = Array::new();
    out.push(&JsValue::from_str(env!("CARGO_PKG_VERSION")));
    out.push(&JsValue::from_str(if cfg!(debug_assertions) { "debug" } else { "release" }));
    out
}
