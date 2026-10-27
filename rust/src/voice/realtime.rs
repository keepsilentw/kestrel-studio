//! The upstream leg of a voice call: a WebSocket client to the realtime model.
//!
//! Split so the risky parts are testable without a network. `realtime_url`,
//! `build_handshake_request` and `parse_handshake_response` are pure, and the
//! response parser is what decides whether a connection is actually usable. What
//! is left over is the TLS dial itself, which cannot be tested here at all.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use base64::Engine;

use crate::config::random_bytes;
use crate::net::ws::accept_key;

/// Turns the provider's HTTP base URL into the realtime endpoint.
///
/// The same host serves four HTTP paths (chat, image, video, task) and this fifth
/// one over WebSocket, so it is derived from the configured base URL rather than
/// hard-coded: switching provider has to move both surfaces together.
///
/// The base URL's own path is **replaced**, not appended to - `new
/// URL('/api-ws/v1/realtime', baseUrl)` in the Nest version resolves an absolute
/// path against the origin. Keeping the prefix there would have pointed at a path
/// the provider does not serve.
pub fn realtime_url(base_url: &str, model: &str) -> Option<String> {
    let mut parsed = url::Url::parse(base_url).ok()?;
    match parsed.scheme() {
        "https" => { let _ = parsed.set_scheme("wss"); }
        "http" => { let _ = parsed.set_scheme("ws"); }
        _ => return None,
    }
    parsed.set_path("/api-ws/v1/realtime");
    parsed.set_query(Some(&format!("model={}", percent_encode(model))));
    let mut out = parsed.to_string();
    // A trailing slash the base URL carried survives into `to_string()`; the
    // endpoint takes none.
    while out.ends_with('/') {
        out.pop();
    }
    Some(out)
}

fn percent_encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(*byte as char),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// The client's `Sec-WebSocket-Key`: 16 random bytes, base64'd. It carries no
/// meaning beyond "pick something you can echo a signature of".
pub fn client_key() -> String {
    base64::engine::general_purpose::STANDARD.encode(random_bytes(16))
}

/// The upgrade request, as bytes on the wire.
pub fn build_handshake_request(path_and_query: &str, host: &str, key: &str, bearer: &str) -> Vec<u8> {
    format!(
        "GET {path_and_query} HTTP/1.1\r\n\
         Host: {host}\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Key: {key}\r\n\
         Sec-WebSocket-Version: 13\r\n\
         Authorization: Bearer {bearer}\r\n\
         \r\n"
    )
    .into_bytes()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpstreamError {
    /// The response was not 101; the text is the status line, kept for the log.
    Rejected(String),
    Malformed(String),
    /// The echoed signature did not match our key: the peer is not speaking the
    /// protocol, or something in between rewrote the headers.
    BadAccept,
}

/// Reads the response head and checks the three things that make the socket ours:
/// 101, the upgrade headers, and a `Sec-WebSocket-Accept` that is exactly the
/// signature of the key we sent.
pub fn parse_handshake_response(head: &str, sent_key: &str) -> Result<(), UpstreamError> {
    let mut lines = head.lines();
    let status = lines
        .next()
        .ok_or_else(|| UpstreamError::Malformed("empty response".into()))?;
    if !status.starts_with("HTTP/1.1 101") && !status.starts_with("HTTP/1.0 101") {
        return Err(UpstreamError::Rejected(status.trim().to_string()));
    }

    let mut upgrade = None;
    let mut connection = None;
    let mut accept = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        match name.trim().to_ascii_lowercase().as_str() {
            "upgrade" => upgrade = Some(value.trim().to_ascii_lowercase()),
            "connection" => connection = Some(value.trim().to_ascii_lowercase()),
            "sec-websocket-accept" => accept = Some(value.trim().to_string()),
            _ => {}
        }
    }

    if upgrade.as_deref() != Some("websocket") {
        return Err(UpstreamError::Malformed("no Upgrade: websocket".into()));
    }
    let connection = connection.ok_or_else(|| UpstreamError::Malformed("no Connection header".into()))?;
    // `Connection` is a list; proxies append to it.
    if !connection.split(',').any(|part| part.trim() == "upgrade") {
        return Err(UpstreamError::Malformed(format!("Connection: {connection}")));
    }
    let expected =
        accept_key(sent_key).ok_or_else(|| UpstreamError::Malformed("our own key was refused".into()))?;
    if accept.as_deref() != Some(expected.as_str()) {
        return Err(UpstreamError::BadAccept);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_realtime_url_reuses_the_provider_host_and_default_ports_disappear() {
        assert_eq!(
            realtime_url("https://token-plan.cn-beijing.maas.aliyuncs.com", "qwen-audio-3.0-realtime-plus").as_deref(),
            Some("wss://token-plan.cn-beijing.maas.aliyuncs.com/api-ws/v1/realtime?model=qwen-audio-3.0-realtime-plus")
        );
        assert_eq!(
            realtime_url("http://127.0.0.1:8848/base", "m").as_deref(),
            Some("ws://127.0.0.1:8848/api-ws/v1/realtime?model=m"),
            "a non-default port stays; the base path is replaced, not appended"
        );
        assert_eq!(
            realtime_url("https://example.test:443/", "a b").as_deref(),
            Some("wss://example.test/api-ws/v1/realtime?model=a%20b"),
            "a space in a model name must not break the request line"
        );
        assert_eq!(realtime_url("ftp://example.test", "m"), None, "no such scheme");
        assert_eq!(realtime_url("not a url", "m"), None);
    }

    #[test]
    fn the_client_key_is_a_base64_secret_the_server_can_sign() {
        let first = client_key();
        let second = client_key();
        assert_eq!(first.len(), 24, "16 bytes in standard base64");
        assert_ne!(first, second);
        assert!(accept_key(&first).is_some(), "the key we send must be signable");
        assert!(accept_key(&format!("{first}!")).is_none(), "a corrupt key is refused");
    }

    #[test]
    fn the_request_carries_every_mandatory_field_on_one_line_each() {
        let request = String::from_utf8(build_handshake_request(
            "/api-ws/v1/realtime?model=m",
            "provider.test",
            "dGVzdA==",
            "sk-test",
        ))
        .expect("ascii");
        assert!(
            request.starts_with("GET /api-ws/v1/realtime?model=m HTTP/1.1\r\n"),
            "{request}"
        );
        for line in [
            "Host: provider.test",
            "Upgrade: websocket",
            "Connection: Upgrade",
            "Sec-WebSocket-Key: dGVzdA==",
            "Sec-WebSocket-Version: 13",
            "Authorization: Bearer sk-test",
        ] {
            assert!(request.contains(&format!("{line}\r\n")), "missing {line} in {request}");
        }
        assert!(request.ends_with("\r\n\r\n"), "the head must terminate");
    }

    fn head(extra: &[&str]) -> String {
        let mut lines = vec!["HTTP/1.1 101 Switching Protocols"];
        lines.extend(extra.iter().copied());
        format!("{}\r\n", lines.join("\r\n"))
    }

    #[test]
    fn a_good_handshake_is_accepted_with_the_matching_signature() {
        let key = "dGVzdA==";
        let accept = accept_key(key).expect("accept");
        let response = head(&[
            "Upgrade: WebSocket",
            "Connection: keep-alive, Upgrade",
            &format!("Sec-WebSocket-Accept: {accept}"),
        ]);
        assert_eq!(
            parse_handshake_response(&response, key),
            Ok(()),
            "header names are case-insensitive and Connection is a list"
        );
    }

    #[test]
    fn anything_that_is_not_101_is_rejected_with_the_status_kept_for_the_log() {
        let key = "dGVzdA==";
        assert_eq!(
            parse_handshake_response("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n", key),
            Err(UpstreamError::Rejected("HTTP/1.1 200 OK".into())),
            "a provider that answers a request instead of upgrading is not a socket"
        );
        assert_eq!(
            parse_handshake_response("", key),
            Err(UpstreamError::Malformed("empty response".into()))
        );
    }

    #[test]
    fn a_mismatched_accept_or_missing_upgrade_headers_fail() {
        let key = "dGVzdA==";
        assert_eq!(
            parse_handshake_response(&head(&["Connection: Upgrade"]), key),
            Err(UpstreamError::Malformed("no Upgrade: websocket".into())),
            "the 101 alone proves nothing"
        );

        let wrong_connection = head(&[
            "Upgrade: websocket",
            "Connection: keep-alive",
            &format!("Sec-WebSocket-Accept: {}", accept_key(key).expect("accept")),
        ]);
        assert!(matches!(
            parse_handshake_response(&wrong_connection, key),
            Err(UpstreamError::Malformed(_))
        ));

        let wrong_accept = head(&[
            "Upgrade: websocket",
            "Connection: Upgrade",
            "Sec-WebSocket-Accept: aW52YWxpZA==",
        ]);
        assert_eq!(parse_handshake_response(&wrong_accept, key), Err(UpstreamError::BadAccept));
    }

}
/// Outbound message shapes. Field names are copied from `realtime-client.ts`;
/// several of them are load-bearing in ways a guess would not be - `turn_detection`
/// must be `null` and not the string `'none'`, and the transcript model is pinned
/// because the transcript is what the UI shows.
pub mod message {
    use serde_json::{json, Value};

    pub fn session_update(instructions: &str, tools: Vec<Value>) -> Value {
        json!({
            "type": "session.update",
            "session": {
                "modalities": ["audio", "text"],
                "instructions": instructions,
                "tools": tools,
                "tool_choice": "auto",
                "turn_detection": null,
                "input_audio_transcription": { "model": "fun-asr" }
            }
        })
    }

    /// One chunk of the utterance: base64 of raw PCM, 16-bit signed, mono, 24 kHz.
    pub fn append_audio(pcm: &str) -> Value {
        json!({ "type": "input_audio_buffer.append", "audio": pcm })
    }

    pub fn commit() -> Value {
        json!({ "type": "input_audio_buffer.commit" })
    }

    pub fn response_create() -> Value {
        json!({ "type": "response.create" })
    }

    /// A tool's result. The caller waits for the server to acknowledge this before
    /// `response_create`: sending both in one tick makes the model re-issue the
    /// identical tool call (docs/verification.md §3.2).
    pub fn function_call_output(call_id: &str, output: &str) -> Value {
        json!({
            "type": "conversation.item.create",
            "item": { "type": "function_call_output", "call_id": call_id, "output": output }
        })
    }
}

/// The upstream's own event names, normalised. Nothing upstream leaks past this
/// enum - the browser sees `voice::protocol::ServerFrame` instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RealtimeEvent {
    /// The socket is up and the session may be configured.
    SessionCreated,
    /// The configuration was accepted: audio may start flowing.
    Ready,
    Transcript(String),
    AssistantText(String),
    Audio(String),
    FunctionCall { call_id: String, name: String, arguments: String },
    /// The model finished a turn; the browser may press the button again.
    ResponseDone,
    /// A `conversation.item.created` ack: the tool result landed, so
    /// `response.create` may follow. Waiting for this is what stops the model
    /// re-issuing the identical tool call.
    ItemCreated,
    Failed(String),
    Closed { code: u16, reason: String },
}

/// The upstream event switch, with the state the Nest client keeps.
///
/// Two kinds of stitching need memory: the assistant's text arrives as deltas and
/// is only reported at `response.audio_transcript.done`, and a function call's
/// arguments arrive as deltas that carry `item_id` rather than `call_id`, so they
/// are routed to the most recently opened call (`output_item.added` always
/// precedes its deltas).
#[derive(Default)]
pub struct UpstreamStitch {
    assistant_text: String,
    call_args: std::collections::HashMap<String, String>,
    active_call_id: Option<String>,
}

impl UpstreamStitch {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the events this frame produced - usually none, one, or a close.
    ///
    /// A frame that is not JSON produces nothing rather than failing: a malformed
    /// upstream frame must not end a live call.
    pub fn handle(&mut self, raw: &str) -> Vec<RealtimeEvent> {
        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(raw) else {
            return Vec::new();
        };
        let Some(kind) = parsed.get("type").and_then(serde_json::Value::as_str) else {
            return Vec::new();
        };
        let text = |key: &str| -> String {
            parsed.get(key).and_then(serde_json::Value::as_str).unwrap_or_default().to_string()
        };
        let item = |key: &str| -> Option<serde_json::Value> { parsed.get(key).cloned() };
        let field = |value: &serde_json::Value, key: &str| -> String {
            value.get(key).and_then(serde_json::Value::as_str).unwrap_or_default().to_string()
        };

        match kind {
            "session.created" => vec![RealtimeEvent::SessionCreated],
            "session.updated" => vec![RealtimeEvent::Ready],
            "conversation.item.input_audio_transcription.completed" => {
                let transcript = text("transcript");
                (!transcript.is_empty())
                    .then_some(RealtimeEvent::Transcript(transcript))
                    .into_iter()
                    .collect()
            }
            "response.audio.delta" => {
                let delta = text("delta");
                (!delta.is_empty()).then_some(RealtimeEvent::Audio(delta)).into_iter().collect()
            }
            "response.audio_transcript.delta" => {
                self.assistant_text.push_str(&text("delta"));
                Vec::new()
            }
            "response.audio_transcript.done" => {
                let done = text("transcript");
                let message = if done.is_empty() { std::mem::take(&mut self.assistant_text) } else { done };
                self.assistant_text.clear();
                (!message.is_empty()).then_some(RealtimeEvent::AssistantText(message)).into_iter().collect()
            }
            "response.output_item.added" => {
                if let Some(item) = item("item") {
                    if field(&item, "type") == "function_call" {
                        let call_id = field(&item, "call_id");
                        self.call_args.insert(call_id.clone(), String::new());
                        self.active_call_id = Some(call_id);
                    }
                }
                Vec::new()
            }
            "response.function_call_arguments.delta" => {
                // Routed to the call opened most recently, because this frame
                // identifies the item, not the call.
                if let Some(call_id) = self.active_call_id.clone() {
                    let entry = self.call_args.entry(call_id).or_default();
                    entry.push_str(&text("delta"));
                }
                Vec::new()
            }
            "response.output_item.done" => {
                let Some(item) = item("item") else { return Vec::new() };
                if field(&item, "type") != "function_call" {
                    return Vec::new();
                }
                let call_id = field(&item, "call_id");
                let streamed = self.call_args.remove(&call_id).unwrap_or_default();
                if self.active_call_id.as_deref() == Some(call_id.as_str()) {
                    self.active_call_id = None;
                }
                let arguments = if streamed.is_empty() {
                    let own = field(&item, "arguments");
                    if own.is_empty() { "{}".to_string() } else { own }
                } else {
                    streamed
                };
                vec![RealtimeEvent::FunctionCall { call_id, name: field(&item, "name"), arguments }]
            }
            "conversation.item.created" => vec![RealtimeEvent::ItemCreated],
            "response.done" => vec![RealtimeEvent::ResponseDone],
            "error" => {
                // The message may ride either the nested error object or the frame
                // itself; the fixed sentence is the last resort, as in the Nest
                // client, so a failure never reaches the browser as an empty string.
                let nested = item("error").map(|value| field(&value, "message")).unwrap_or_default();
                let message = if !nested.is_empty() {
                    nested
                } else if !text("message").is_empty() {
                    text("message")
                } else {
                    "语音服务报错".to_string()
                };
                vec![RealtimeEvent::Failed(message)]
            }
            // Anything else is a name this port has not read from the Nest client.
            _ => Vec::new(),
        }
    }

    /// The call whose argument deltas are arriving, exposed for tests.
    pub fn active_call(&self) -> Option<&str> {
        self.active_call_id.as_deref()
    }
}

/// Maps one upstream frame without any stitching state.
///
/// For the stateless events only; a session uses [`UpstreamStitch`].
pub fn map_inbound_event(raw: &str) -> Option<RealtimeEvent> {
    UpstreamStitch::new().handle(raw).into_iter().next()
}

#[cfg(test)]
mod vocabulary {
    use super::{map_inbound_event, message, RealtimeEvent, UpstreamStitch};
    use serde_json::json;

    #[test]
    fn the_session_update_carries_the_fields_the_endpoint_requires() {
        let update = message::session_update("说中文", vec![json!({"type": "function"})]);
        assert_eq!(update["type"], json!("session.update"));
        assert_eq!(update["session"]["modalities"], json!(["audio", "text"]));
        assert_eq!(update["session"]["tool_choice"], json!("auto"));
        // null, not "none": the string form is rejected by the endpoint.
        assert!(update["session"]["turn_detection"].is_null());
        assert_eq!(update["session"]["input_audio_transcription"]["model"], json!("fun-asr"));
        assert_eq!(update["session"]["instructions"], json!("说中文"));
        assert_eq!(update["session"]["tools"].as_array().expect("tools").len(), 1);
    }

    #[test]
    fn the_audio_and_call_messages_use_the_upstream_names() {
        assert_eq!(message::append_audio("AAA="), json!({"type":"input_audio_buffer.append","audio":"AAA="}));
        assert_eq!(message::commit(), json!({"type":"input_audio_buffer.commit"}));
        assert_eq!(message::response_create(), json!({"type":"response.create"}));
        assert_eq!(
            message::function_call_output("c1", "asset 7"),
            json!({"type":"conversation.item.create","item":{"type":"function_call_output","call_id":"c1","output":"asset 7"}})
        );
    }

    #[test]
    fn assistant_text_accumulates_and_the_done_frame_wins_over_the_deltas() {
        let mut stitch = UpstreamStitch::default();
        assert!(stitch.handle(r#"{"type":"response.audio_transcript.delta","delta":"你看"}"#).is_empty());
        assert!(stitch.handle(r#"{"type":"response.audio_transcript.delta","delta":"这只鸟"}"#).is_empty());
        // The done frame carries its own transcript when the endpoint provides one.
        assert_eq!(
            stitch.handle(r#"{"type":"response.audio_transcript.done","transcript":"你看这只鸟"}"#),
            vec![RealtimeEvent::AssistantText("你看这只鸟".into())]
        );
        // ...and falls back to what was streamed when it does not.
        stitch.handle(r#"{"type":"response.audio_transcript.delta","delta":"第二段"}"#);
        assert_eq!(
            stitch.handle(r#"{"type":"response.audio_transcript.done"}"#),
            vec![RealtimeEvent::AssistantText("第二段".into())]
        );
        assert!(stitch.handle(r#"{"type":"response.audio_transcript.done"}"#).is_empty(), "the buffer was cleared");
    }

    #[test]
    fn an_audio_delta_survives_and_an_empty_one_does_not() {
        let mut stitch = UpstreamStitch::default();
        assert_eq!(
            stitch.handle(r#"{"type":"response.audio.delta","delta":"AAA="}"#),
            vec![RealtimeEvent::Audio("AAA=".into())]
        );
        assert!(stitch.handle(r#"{"type":"response.audio.delta","delta":""}"#).is_empty());
    }

    #[test]
    fn function_call_arguments_stitch_onto_the_call_that_was_opened_last() {
        let mut stitch = UpstreamStitch::default();
        // A delta before any call was opened is dropped, not attributed to nothing.
        assert!(stitch.handle(r#"{"type":"response.function_call_arguments.delta","delta":"{}"}"#).is_empty());

        stitch.handle(r#"{"type":"response.output_item.added","item":{"type":"function_call","call_id":"c1","name":"generate_image"}}"#);
        assert_eq!(stitch.active_call(), Some("c1"));
        stitch.handle(r#"{"type":"response.function_call_arguments.delta","delta":"{\"prompt\":"}"#);
        stitch.handle(r#"{"type":"response.function_call_arguments.delta","delta":"\"红隼\"}"}"#);
        assert_eq!(
            stitch.handle(r#"{"type":"response.output_item.done","item":{"type":"function_call","call_id":"c1","name":"generate_image","arguments":"{}"}}"#),
            vec![RealtimeEvent::FunctionCall {
                call_id: "c1".into(),
                name: "generate_image".into(),
                arguments: r#"{"prompt":"红隼"}"#.into(),
            }],
            "the streamed arguments win over the item's own copy"
        );
        assert_eq!(stitch.active_call(), None, "the call is closed");

        // With nothing streamed, the item's own arguments are used...
        stitch.handle(r#"{"type":"response.output_item.added","item":{"type":"function_call","call_id":"c2","name":"x"}}"#);
        assert_eq!(
            stitch.handle(r#"{"type":"response.output_item.done","item":{"type":"function_call","call_id":"c2","name":"x","arguments":"{\"a\":1}"}}"#),
            vec![RealtimeEvent::FunctionCall { call_id: "c2".into(), name: "x".into(), arguments: r#"{"a":1}"#.into() }]
        );
        // ...and if it carries none either, the model gets an empty object.
        stitch.handle(r#"{"type":"response.output_item.added","item":{"type":"function_call","call_id":"c3","name":"x"}}"#);
        let last = stitch.handle(r#"{"type":"response.output_item.done","item":{"type":"function_call","call_id":"c3","name":"x"}}"#);
        assert_eq!(last, vec![RealtimeEvent::FunctionCall { call_id: "c3".into(), name: "x".into(), arguments: "{}".into() }]);
    }

    #[test]
    fn non_function_call_items_and_unparsable_frames_produce_nothing() {
        let mut stitch = UpstreamStitch::default();
        assert!(stitch.handle(r#"{"type":"response.output_item.done","item":{"type":"message"}}"#).is_empty());
        assert!(stitch.handle("not json").is_empty());
        assert!(stitch.handle("[]").is_empty());
        assert!(stitch.handle(r#"{"noType":1}"#).is_empty());
    }

    #[test]
    fn the_error_frame_falls_back_through_its_three_message_sources() {
        let mut stitch = UpstreamStitch::default();
        assert_eq!(
            stitch.handle(r#"{"type":"error","error":{"message":"额度不足"}}"#),
            vec![RealtimeEvent::Failed("额度不足".into())]
        );
        assert_eq!(
            stitch.handle(r#"{"type":"error","message":"上游 502"}"#),
            vec![RealtimeEvent::Failed("上游 502".into())]
        );
        assert_eq!(
            stitch.handle(r#"{"type":"error"}"#),
            vec![RealtimeEvent::Failed("语音服务报错".into())],
            "a failure never reaches the browser as an empty string"
        );
        // The nested one wins when both are present.
        assert_eq!(
            stitch.handle(r#"{"type":"error","error":{"message":"内层"},"message":"外层"}"#),
            vec![RealtimeEvent::Failed("内层".into())]
        );
    }

    #[test]
    fn the_turn_end_and_the_item_ack_are_reported() {
        let mut stitch = UpstreamStitch::default();
        assert_eq!(stitch.handle(r#"{"type":"response.done"}"#), vec![RealtimeEvent::ResponseDone]);
        assert_eq!(
            stitch.handle(r#"{"type":"conversation.item.created","item":{}}"#),
            vec![RealtimeEvent::ItemCreated]
        );
    }

    #[test]
    fn only_the_verifiable_upstream_events_map_and_nothing_else_invents_one() {
        assert_eq!(map_inbound_event(r#"{"type":"session.created"}"#), Some(RealtimeEvent::SessionCreated));
        assert_eq!(map_inbound_event(r#"{"type":"session.updated"}"#), Some(RealtimeEvent::Ready));
        assert_eq!(
            map_inbound_event(r#"{"type":"conversation.item.input_audio_transcription.completed","transcript":"你好"}"#),
            Some(RealtimeEvent::Transcript("你好".into()))
        );
        // An empty transcript is no transcript.
        assert_eq!(
            map_inbound_event(r#"{"type":"conversation.item.input_audio_transcription.completed","transcript":""}"#),
            None
        );
        // Names this port has not read from the Nest client stay unmapped: an
        // unknown upstream event is silently ignored, never renamed into a guess.
        assert_eq!(map_inbound_event(r#"{"type":"response.content_part.added"}"#), None);
        assert_eq!(map_inbound_event(r#"{"type":"rate_limits.updated"}"#), None);
        assert_eq!(map_inbound_event("not json"), None);
        assert_eq!(map_inbound_event("[]"), None);
    }
}

/// How long to wait for the socket to come up before giving up.
pub const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// How long to wait for the server to acknowledge a submitted tool result before
/// proceeding anyway. See `message::function_call_output` for why the wait exists.
pub const ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// The host and port to dial, out of the realtime URL.
///
/// Split from the dial because this part is arithmetic, not network: the authority
/// is what TLS must validate the certificate against, and a mistake here is a
/// handshake failure that looks like a certificate problem.
pub fn endpoint(url: &str) -> Option<(String, u16)> {
    let parsed = url::Url::parse(url).ok()?;
    match parsed.scheme() {
        "wss" | "https" => {}
        "ws" | "http" => {}
        _ => return None,
    }
    let host = parsed.host_str()?.to_string();
    let port = parsed.port_or_known_default()?;
    Some((host, port))
}

/// Reads the response head: bytes up to and including the blank line that ends it.
///
/// One byte at a time, because the byte after the head is the first byte of the
/// WebSocket frames and must not be swallowed here. Bounded, so a peer that never
/// terminates the head cannot make the session grow.
pub async fn read_response_head<R: AsyncReadExt + Unpin>(
    reader: &mut R,
) -> Result<String, UpstreamError> {
    const MAX_HEAD: usize = 16 << 10;
    let mut bytes = Vec::with_capacity(256);
    let mut buf = [0u8; 1];
    loop {
        let read = reader
            .read(&mut buf)
            .await
            .map_err(|error| UpstreamError::Malformed(format!("reading the response head: {error}")))?;
        if read == 0 {
            return Err(UpstreamError::Malformed(format!(
                "the peer closed the connection mid-head: {}",
                String::from_utf8_lossy(&bytes).trim()
            )));
        }
        bytes.push(buf[0]);
        if bytes.ends_with(b"\r\n\r\n") {
            return Ok(String::from_utf8_lossy(&bytes).into_owned());
        }
        if bytes.len() >= MAX_HEAD {
            return Err(UpstreamError::Malformed("the response head is unreasonably long".into()));
        }
    }
}

/// Opens the upstream socket: TCP, TLS, then the WebSocket handshake whose request
/// and response are the two functions tested above.
///
/// Unverified in this environment: it dials, and this box can neither reach the
/// provider nor bind a loopback port to fake it. Everything it is built from is
/// tested, so a failure here is the network or the provider, not the framing.
pub async fn connect(base_url: &str, model: &str, api_key: &str) -> Result<UpstreamSocket, UpstreamError> {
    use tokio_rustls::rustls::pki_types::ServerName;
    use tokio_rustls::TlsConnector;

    let url = realtime_url(base_url, model).ok_or_else(|| {
        UpstreamError::Malformed(format!("cannot derive a realtime URL from {base_url}"))
    })?;
    let (host, port) = endpoint(&url).ok_or_else(|| UpstreamError::Malformed(url.clone()))?;
    let path_and_query = url::Url::parse(&url)
        .map(|parsed| format!("{}?{}", parsed.path(), parsed.query().unwrap_or_default()))
        .unwrap_or_else(|_| "/api-ws/v1/realtime".to_string());

    let roots = rustls_root_store();
    let config = tokio_rustls::rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connector = TlsConnector::from(std::sync::Arc::new(config));

    let opened = tokio::time::timeout(CONNECT_TIMEOUT, async {
        let tcp = tokio::net::TcpStream::connect((host.as_str(), port))
            .await
            .map_err(|error| UpstreamError::Rejected(format!("connecting to {host}:{port}: {error}")))?;
        let name = ServerName::try_from(host.clone())
            .map_err(|error| UpstreamError::Malformed(format!("bad server name: {error}")))?;
        let mut tls = connector
            .connect(name, tcp)
            .await
            .map_err(|error| UpstreamError::Rejected(format!("TLS with {host}: {error}")))?;

        let key = client_key();
        tls.write_all(&build_handshake_request(&path_and_query, &host, &key, api_key))
            .await
            .map_err(|error| UpstreamError::Malformed(format!("sending the handshake: {error}")))?;
        tls.flush().await.ok();

        let (reader, writer) = tokio::io::split(tls);
        let mut reader = reader;
        let head = read_response_head(&mut reader).await?;
        parse_handshake_response(&head, &key)?;
        // The halves stay split: each one is owned by exactly one pump task, which
        // is the shape `relay::pump_in` and `relay::pump_out` already take.
        Ok::<_, UpstreamError>(UpstreamSocket { reader, writer })
    })
    .await
    .map_err(|_| UpstreamError::Rejected("语音会话建立超时".to_string()))??;

    Ok(opened)
}

/// The trust anchors. `webpki-roots` rather than the platform store: the container
/// this runs in has no system CA bundle it can be asked to read.
fn rustls_root_store() -> tokio_rustls::rustls::RootCertStore {
    let mut store = tokio_rustls::rustls::RootCertStore::empty();
    store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    store
}

/// The upstream socket, after TLS and the WebSocket handshake.
///
/// Split deliberately: `relay::pump_in` owns the reader and `relay::pump_out` owns
/// the writer, so neither side needs a lock and a frame cannot be written halfway
/// into another frame.
pub struct UpstreamSocket {
    pub reader: tokio::io::ReadHalf<tokio_rustls::client::TlsStream<tokio::net::TcpStream>>,
    pub writer: tokio::io::WriteHalf<tokio_rustls::client::TlsStream<tokio::net::TcpStream>>,
}

#[cfg(test)]
mod dial_pieces {
    use super::{endpoint, parse_handshake_response, read_response_head, UpstreamError};
    use tokio::io::AsyncWriteExt;

    #[test]
    fn the_dial_target_keeps_the_host_tls_must_validate_against() {
        assert_eq!(
            endpoint("wss://provider.test/api-ws/v1/realtime?model=m"),
            Some(("provider.test".to_string(), 443))
        );
        assert_eq!(
            endpoint("ws://127.0.0.1:8849/api-ws/v1/realtime"),
            Some(("127.0.0.1".to_string(), 8849))
        );
        assert_eq!(endpoint("ftp://provider.test"), None);
        assert_eq!(endpoint("nonsense"), None);
    }

    #[tokio::test]
    async fn the_head_reader_stops_at_the_blank_line_and_returns_every_byte_of_it() {
        let (mut client, server) = tokio::io::duplex(64);
        let mut server = server;
        let reader = tokio::spawn(async move { read_response_head(&mut server).await });

        // Deliberately split across writes, and with a byte after the head that
        // must be left for the frame decoder.
        client.write_all(b"HTTP/1.1 101 Switching Proto").await.unwrap();
        client.write_all(b"cols\r\n\r\n").await.unwrap();
        client.write_all(b"\x81\x02hi").await.unwrap();
        let head = reader.await.unwrap().expect("head");
        assert!(head.ends_with("\r\n\r\n"), "{head:?}");
        assert!(!head.as_bytes().contains(&0x81), "the first frame byte must not be eaten: {head:?}");
        assert!(
            head.contains("HTTP/1.1 101 Switching Protocols"),
            "the whole head arrives in one piece"
        );
    }

    #[tokio::test]
    async fn a_closed_or_endless_head_is_an_error_not_a_hang() {
        let (client, mut server) = tokio::io::duplex(64);
        drop(client);
        let error = read_response_head(&mut server).await.expect_err("closed");
        assert!(matches!(error, UpstreamError::Malformed(_)), "{error:?}");

        let (mut client, mut server) = tokio::io::duplex(1 << 16);
        let writer = tokio::spawn(async move {
            let byte = [b'H'; 1];
            for _ in 0..(20 << 10) {
                if client.write_all(&byte).await.is_err() {
                    return;
                }
            }
        });
        let error = read_response_head(&mut server).await.expect_err("unbounded");
        assert!(format!("{error:?}").contains("unreasonably long"), "{error:?}");
        writer.abort();
    }

    #[test]
    fn the_response_must_still_be_a_valid_upgrade_after_the_head_arrives() {
        // The two halves compose: whatever the reader returns goes to the parser.
        let head = "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n";
        assert_eq!(
            parse_handshake_response(head, "dGVzdA=="),
            Err(super::UpstreamError::BadAccept),
            "no Sec-WebSocket-Accept is not a handshake we accept"
        );
    }
}
