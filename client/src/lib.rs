//! The browser side of kestrel-studio, in Rust.
//!
//! Layout rule: anything that can be decided without a DOM lives in a plain
//! module (`voice_core`) so `cargo test` can assert it on the host target. The
//! WASM-only modules (DOM, sockets, audio) are gated behind
//! `cfg(target_arch = "wasm32")` and stay thin: they translate browser events
//! into core calls and core effects into browser calls.
//!
//! Why the split is load-bearing: the jsdom suite this replaces (`__tests__/web/`)
//! exists because of a production bug it would have caught — the client opened
//! the socket and never sent the `start` frame, so the page sat on "正在连接…"
//! with a healthy socket, a silent server and a clean console. That bug lives in
//! the ordering of frames, not in the DOM, and the ordering is now asserted
//! without a browser at all.

pub mod voice_core;

/// The conversation page's decisions: replay dedupe, the `resync` rule, the mode
/// read back from storage, and the download re-encode maths.
pub mod chat_core;
pub mod pcm;

// The exports exist only where they can be called: on the host target the same
// module would drag wasm-bindgen into `cargo test` for no reason.
#[cfg(target_arch = "wasm32")]
pub mod wasm;

/// The voice socket: ticket, upgrade, and the frames in and out.
#[cfg(target_arch = "wasm32")]
pub mod wire;

/// The microphone and the playback queue. Nothing here is reachable from the
/// host target, which is why the byte-level work sits in `pcm` instead.
#[cfg(target_arch = "wasm32")]
pub mod audio;

/// The `/voice` page: DOM, hold wiring, and the effect interpreter.
#[cfg(target_arch = "wasm32")]
pub mod voice_page;
