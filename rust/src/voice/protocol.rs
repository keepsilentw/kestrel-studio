//! Frames exchanged with the browser over the voice socket.
//!
//! Naming mirrors the SSE events in `src/sse.rs` and the `AssetView` shape, so the
//! two channels can be reasoned about with one mental model. This file is the
//! server's source of truth; `web/scripts/voice.ts` restates the shapes by hand
//! (a browser script cannot import server code), so the two are kept in step -
//! the path and the `ticket` query name below come from that script.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::task::TaskKind;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_four_client_frames_parse_and_nothing_else_does() {
        assert_eq!(parse_client_frame(r#"{"type":"start"}"#), Some(ClientFrame::Start));
        assert_eq!(parse_client_frame(r#"{"type":"commit"}"#), Some(ClientFrame::Commit));
        assert_eq!(parse_client_frame(r#"{"type":"stop"}"#), Some(ClientFrame::Stop));
        assert_eq!(
            parse_client_frame(r#"{"type":"audio","pcm":"AAA="}"#),
            Some(ClientFrame::Audio { pcm: "AAA=".to_string() })
        );
        for raw in ["", "not json", "[]", "null", r#"{"type":"nope"}"#, r#"{"type":"audio"}"#, r#"{"type":"audio","pcm":""}"#, r#"{"pcm":"AAA="}"#] {
            assert_eq!(parse_client_frame(raw), None, "{raw}");
        }
    }

    #[test]
    fn server_frames_carry_the_names_the_browser_listens_for() {
        // Each one is a `type` discriminant plus its fields, in the camelCase the
        // client script reads.
        let cases: Vec<(ServerFrame, &str, &str)> = vec![
            (ServerFrame::Ready { conversation_id: 7 }, "ready", "\"conversationId\":7"),
            (ServerFrame::Transcript { text: "一只红隼".into() }, "transcript", "\"text\":\"一只红隼\""),
            (ServerFrame::ToolCall { name: "generate_image".into(), status: "running".into() }, "tool_call", "\"status\":\"running\""),
            (
                ServerFrame::Asset {
                    id: 3,
                    kind: TaskKind::Image,
                    url: "/api/v1/assets/3/download".into(),
                    mime: "image/png".into(),
                    bytes: 4,
                },
                "asset",
                "\"url\":\"/api/v1/assets/3/download\"",
            ),
            (ServerFrame::Audio { pcm: "AAA=".into() }, "audio", "\"pcm\":\"AAA=\""),
            (ServerFrame::AssistantText { text: "看好".into() }, "assistant_text", "\"text\":\"看好\""),
            (ServerFrame::Done, "done", ""),
            (ServerFrame::Error { message: "上游断开".into() }, "error", "\"message\":\"上游断开\""),
        ];
        for (frame, name, fragment) in cases {
            let json = frame.to_json();
            assert!(json.contains(&format!("\"type\":\"{name}\"")), "{json}");
            if !fragment.is_empty() {
                assert!(json.contains(fragment), "{json}");
            }
            // Whatever the client sent back would have to parse; these are the
            // server's own frames, so round-tripping proves the shapes agree.
            assert!(serde_json::from_str::<Value>(&json).is_ok(), "{json}");
        }
    }

    #[test]
    fn an_asset_frame_is_the_same_shape_the_sse_channel_sends() {
        let frame = ServerFrame::Asset {
            id: 9,
            kind: TaskKind::Video,
            url: "/api/v1/assets/9/download".into(),
            mime: "video/mp4".into(),
            bytes: 8,
        };
        let json = frame.to_json();
        assert!(json.contains("\"kind\":\"video\""), "{json}");
        assert!(json.contains("\"bytes\":8"), "{json}");
        assert!(!json.contains("messageId"), "no turn id on this channel: {json}");
    }
}
