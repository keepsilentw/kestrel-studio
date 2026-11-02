//! The wire, in one place.
//!
//! Both sides compile against these types now: the server (`rust/`) and the
//! browser client (`client/`, to WASM). Before this crate the shapes lived in
//! `rust/src/voice/protocol.rs` and were **restated by hand** in
//! `web/scripts/voice.ts`, because a browser script cannot import server code.
//! That duplication is what made the voice `start`-frame outage possible
//! (docs/implementation-notes.md §8, docs/verification.md §7): with one source of
//! truth, a renamed field is a compile error on the side that consumes it.
//!
//! Naming mirrors the SSE events in `rust/src/sse.rs` and the `AssetView` shape,
//! so the two channels can be reasoned about with one mental model.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub mod event;
pub mod task;

pub use task::{TaskKind, TaskStatus};

/// Browser → server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientFrame {
    /// The button was pressed: open the upstream session for this call.
    Start,
    /// One chunk of the utterance. Base64 of raw PCM, 16-bit signed, mono, 24 kHz.
    Audio { pcm: String },
    /// The button was released: the utterance is complete.
    Commit,
    Stop,
}

/// Server → browser.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerFrame {
    /// The upstream session is live; the client may start sending audio.
    Ready {
        #[serde(rename = "conversationId")]
        conversation_id: i64,
    },
    /// What the user said, after upstream transcription.
    Transcript { text: String },
    ToolCall { name: String, status: String },
    /// The same fields the SSE `asset` event carries, spelled out because serde
    /// cannot flatten inside a newtype variant.
    #[serde(rename = "asset")]
    Asset {
        id: i64,
        kind: TaskKind,
        url: String,
        mime: String,
        bytes: i64,
    },
    /// Base64 of raw PCM to play back.
    Audio { pcm: String },
    /// What the model said, as text alongside the audio.
    #[serde(rename = "assistant_text")]
    AssistantText { text: String },
    Done,
    Error { message: String },
}

impl ServerFrame {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{\"type\":\"error\"}".to_string())
    }
}

/// Narrows a raw socket message. `None` for anything unrecognised rather than an
/// error: the peer is a browser, and a malformed frame must not take the
/// connection down.
pub fn parse_client_frame(raw: &str) -> Option<ClientFrame> {
    let parsed: Value = serde_json::from_str(raw).ok()?;
    match parsed.get("type").and_then(Value::as_str)? {
        "start" => Some(ClientFrame::Start),
        "commit" => Some(ClientFrame::Commit),
        "stop" => Some(ClientFrame::Stop),
        "audio" => parsed
            .get("pcm")
            .and_then(Value::as_str)
            .filter(|pcm| !pcm.is_empty())
            .map(|pcm| ClientFrame::Audio { pcm: pcm.to_string() }),
        _ => None,
    }
}
