//! The voice wire, re-exported from `kestrel-proto`.
//!
//! The types themselves moved out of this tree because the browser client is now
//! Rust too: `client/` compiles against the same `ClientFrame` / `ServerFrame`,
//! so a renamed field is a compile error on the consuming side instead of a
//! silently mis-parsed frame. The history of that failure is in
//! `docs/implementation-notes.md` §8 (the missing `start` frame).
//!
//! This module keeps the tests: they pin the **wire names** the browser listens
//! for, which is the contract that must not drift even though the types are
//! shared. `docs/architecture.md` §11 is the design.

pub use kestrel_proto::{parse_client_frame, ClientFrame, ServerFrame};

// (The wire-name tests pull `TaskKind` from `crate::task` themselves: this
// module is private to `voice`, so a re-export here would be unreachable.)

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::TaskKind;
    use serde_json::Value;

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
