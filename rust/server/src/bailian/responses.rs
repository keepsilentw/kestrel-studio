use futures_util::StreamExt;
use serde::Serialize;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::common::sse_parser::SseParser;
use crate::config::AppConfig;
use crate::error::AppError;

use super::InputItem;

/// A tool the model may call. `parameters` is a JSON Schema object.
#[derive(Debug, Clone, Serialize)]
pub struct FunctionTool {
    pub r#type: &'static str,
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

impl FunctionTool {
    pub fn new(name: &str, description: &str, parameters: serde_json::Value) -> Self {
        Self {
            r#type: "function",
            name: name.to_string(),
            description: description.to_string(),
            parameters,
        }
    }
}

/// Responses API input items. The function_call / function_call_output pair is
/// how a tool round is replayed: the call must be echoed back before its result,
/// since this loop is stateless (no `previous_response_id`).
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum RequestItem {
    Message(InputItem),
    FunctionCall {
        r#type: &'static str,
        call_id: String,
        name: String,
        arguments: String,
    },
    FunctionCallOutput {
        r#type: &'static str,
        call_id: String,
        output: String,
    },
}

impl RequestItem {
    pub fn user(content: &str) -> Self {
        Self::Message(InputItem::user(content))
    }

    pub fn assistant(content: &str) -> Self {
        Self::Message(InputItem::assistant(content))
    }

    pub fn call(call_id: &str, name: &str, arguments: &str) -> Self {
        Self::FunctionCall {
            r#type: "function_call",
            call_id: call_id.to_string(),
            name: name.to_string(),
            arguments: arguments.to_string(),
        }
    }

    pub fn output(call_id: &str, output: &str) -> Self {
        Self::FunctionCallOutput {
            r#type: "function_call_output",
            call_id: call_id.to_string(),
            output: output.to_string(),
        }
    }
}

/// One increment of the model's output.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    Reasoning(String),
    Text(String),
    FunctionCall {
        call_id: String,
        name: String,
        arguments: String,
    },
    /// The provider failed mid-stream. The loop turns it into a `turn_error`.
    Failed(String),
}

pub struct StreamRequest<'a> {
    pub input: Vec<RequestItem>,
    pub tools: Vec<FunctionTool>,
    pub instructions: Option<&'a str>,
}

/// Hand-written fetch plus SSE parsing over the Responses endpoint.
///
/// Deliberately not the `openai` SDK: the endpoint attaches a non-standard
/// `:HTTP_STATUS/200` comment and an `id:N` line to every frame, and its event
/// model is fully mapped in docs/verification.md §1, so the SDK would only add
/// uncertainty.
pub struct ResponsesClient;

impl ResponsesClient {
    /// Streams one completion. Dropping the returned stream cancels the request,
    /// which is what the Nest version did with an `AbortSignal`.
    pub async fn stream(
        http: &reqwest::Client,
        config: &AppConfig,
        api_key: &str,
        request: StreamRequest<'_>,
    ) -> Result<ReceiverStream<StreamEvent>, AppError> {
        let url = format!("{}/compatible-mode/v1/responses", config.bailian.base_url);
        let body = build_body(config, &request);

        let response = http
            .post(&url)
            .bearer_auth(api_key)
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .json(&body)
            .send()
            .await
            .map_err(|error| AppError::internal(anyhow::anyhow!("Bailian request: {error}")))?;

        let status = response.status();
        if !status.is_success() {
            let raw = response
                .text()
                .await
                .unwrap_or_default();
            let truncated: String = raw.chars().take(300).collect();
            return Err(AppError::internal(anyhow::anyhow!(
                "Bailian request failed (HTTP {}): {}",
                status.as_u16(),
                if truncated.is_empty() { "no body" } else { &truncated }
            )));
        }

        let (sender, receiver) = mpsc::channel::<StreamEvent>(64);
        let mut bytes = response.bytes_stream();
        tokio::spawn(async move {
            let mut parser = SseParser::new();
            let mut pending: Option<PendingCall> = None;
            while let Some(chunk) = bytes.next().await {
                let chunk = match chunk {
                    Ok(value) => value,
                    Err(error) => {
                        let _ = sender
                            .send(StreamEvent::Failed(format!("stream interrupted: {error}")))
                            .await;
                        return;
                    }
                };
                for frame in parser.push(&chunk) {
                    if !handle_frame(&frame.event, &frame.data, &mut pending, &sender).await {
                        return;
                    }
                }
            }
            if let Some(frame) = parser.finish() {
                handle_frame(&frame.event, &frame.data, &mut pending, &sender).await;
            }
        });

        Ok(ReceiverStream::new(receiver))
    }
}

fn build_body(config: &AppConfig, request: &StreamRequest<'_>) -> serde_json::Value {
    let mut body = serde_json::json!({
        "model": config.bailian.chat_model,
        "input": request.input,
        "stream": true,
        "reasoning": { "effort": config.bailian.reasoning_effort.as_str() },
    });
    if !request.tools.is_empty() {
        body["tools"] = serde_json::to_value(&request.tools).expect("tool specs are JSON");
        // Fixed to `auto`: a mode narrows which tools are visible, it never forces
        // a call, so an image turn may legitimately answer with text only.
        body["tool_choice"] = serde_json::json!("auto");
    }
    if let Some(instructions) = request.instructions {
        body["instructions"] = serde_json::json!(instructions);
    }
    body
}

struct PendingCall {
    call_id: String,
    name: String,
    arguments: String,
}

/// Returns false when the stream must stop (a failed frame, or a dropped reader).
async fn handle_frame(
    event: &Option<String>,
    data: &str,
    pending: &mut Option<PendingCall>,
    sender: &mpsc::Sender<StreamEvent>,
    ) -> bool {
    let Some(name) = event else { return true };
    let Ok(payload) = serde_json::from_str::<serde_json::Value>(data) else {
        tracing::warn!("skipping unparsable SSE frame for event {name}");
        return true;
    };

    let delta = |key: &str| payload.get(key).and_then(serde_json::Value::as_str);

    let emitted = match name.as_str() {
        "response.reasoning_text.delta" => delta("delta")
            .filter(|value| !value.is_empty())
            .map(|value| StreamEvent::Reasoning(value.to_string())),
        "response.output_text.delta" => delta("delta")
            .filter(|value| !value.is_empty())
            .map(|value| StreamEvent::Text(value.to_string())),
        "response.output_item.added" => {
            let item = payload.get("item");
            if item.and_then(|value| value.get("type")).and_then(serde_json::Value::as_str)
                == Some("function_call")
            {
                *pending = Some(PendingCall {
                    call_id: string_of(item.and_then(|value| value.get("call_id"))),
                    name: string_of(item.and_then(|value| value.get("name"))),
                    arguments: String::new(),
                });
            }
            None
        }
        "response.function_call_arguments.delta" => {
            if let (Some(call), Some(delta)) = (pending.as_mut(), delta("delta")) {
                call.arguments.push_str(delta);
            }
            None
        }
        "response.output_item.done" => {
            let item = payload.get("item");
            let is_call = item
                .and_then(|value| value.get("type"))
                .and_then(serde_json::Value::as_str)
                == Some("function_call");
            match (is_call, pending.take()) {
                (true, Some(call)) => Some(StreamEvent::FunctionCall {
                    call_id: call.call_id,
                    name: call.name,
                    arguments: if call.arguments.is_empty() {
                        string_of(item.and_then(|value| value.get("arguments")))
                    } else {
                        call.arguments
                    },
                }),
                _ => None,
            }
        }
        "response.failed" | "error" => {
            let message = payload
                .get("error")
                .and_then(|value| value.get("message"))
                .and_then(serde_json::Value::as_str)
                .or_else(|| delta("message"))
                .unwrap_or("Bailian reported a failed response.")
                .to_string();
            Some(StreamEvent::Failed(message))
        }
        _ => None,
    };

    match emitted {
        Some(event) => {
            let failed = matches!(event, StreamEvent::Failed(_));
            if sender.send(event).await.is_err() {
                return false; // the reader went away
            }
            !failed
        }
        None => true,
    }
}

fn string_of(value: Option<&serde_json::Value>) -> String {
    match value {
        Some(serde_json::Value::String(text)) => text.clone(),
        Some(other) if !other.is_null() => other.to_string(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc::error::TryRecvError;

    fn config() -> AppConfig {
        AppConfig {
            port: 8848,
            is_production: false,
            session_secret: "s".to_string(),
            database_file: String::new(),
            storage_dir: String::new(),
            public_base_url: "https://example.test".to_string(),
            super_admin: None,
            bailian: crate::config::BailianConfig {
                base_url: "https://bailian.test".to_string(),
                chat_model: "chat-model".to_string(),
                reasoning_effort: crate::config::ReasoningEffort::High,
                video_model_t2v: "t2v".to_string(),
                video_model_i2v: "i2v".to_string(),
                voice_model: "voice".to_string(),
            },
        }
    }

    #[test]
    fn tools_and_instructions_appear_only_when_supplied() {
        let empty = StreamRequest {
            input: vec![RequestItem::user("hi")],
            tools: vec![],
            instructions: None,
        };
        let body = build_body(&config(), &empty);
        assert!(body.get("tools").is_none(), "chat mode advertises nothing");
        assert!(body.get("instructions").is_none());
        assert_eq!(body["stream"], serde_json::json!(true));
        assert_eq!(body["reasoning"]["effort"], serde_json::json!("high"));

        let full = StreamRequest {
            input: vec![
                RequestItem::user("画一只红隼"),
                RequestItem::call("call-1", "generate_image", "{\"prompt\":\"x\"}"),
                RequestItem::output("call-1", "asset 7"),
            ],
            tools: vec![FunctionTool::new(
                "generate_image",
                "文生图",
                serde_json::json!({ "type": "object" }),
            )],
            instructions: Some("规则"),
        };
        let body = build_body(&config(), &full);
        assert_eq!(body["tool_choice"], serde_json::json!("auto"));
        assert_eq!(body["tools"][0]["type"], serde_json::json!("function"));
        assert_eq!(body["instructions"], serde_json::json!("规则"));
        assert_eq!(
            body["input"][1]["type"],
            serde_json::json!("function_call"),
            "the call is echoed back before its result"
        );
        assert_eq!(body["input"][2]["call_id"], serde_json::json!("call-1"));
        assert_eq!(
            body["input"][0]["role"],
            serde_json::json!("user"),
            "message items keep their own shape"
        );
        assert!(body["input"][0].get("type").is_none());
    }

    #[tokio::test]
    async fn frames_become_events_and_a_function_call_is_reassembled() {
        let (sender, mut receiver) = mpsc::channel(8);
        // Driving `handle_frame` directly keeps the provider's event model under
        // test without a live endpoint.
        let frames: [(&str, &str); 5] = [
            ("response.reasoning_text.delta", "{\"delta\":\"想\"}"),
            ("response.output_text.delta", "{\"delta\":\"好的\"}"),
            ("response.output_item.added", "{\"item\":{\"type\":\"function_call\",\"call_id\":\"c1\",\"name\":\"generate_image\"}}"),
            ("response.function_call_arguments.delta", "{\"delta\":\"{\\\"prompt\\\":\\\"x\\\"}\"}"),
            ("response.output_item.done", "{\"item\":{\"type\":\"function_call\",\"call_id\":\"c1\",\"name\":\"generate_image\",\"arguments\":\"{}\"}}"),
        ];
        let mut held: Option<PendingCall> = None;
        for (name, data) in frames {
            assert!(
                handle_frame(&Some(name.to_string()), data, &mut held, &sender)
                    .await,
                "the stream keeps going"
            );
        }
        drop(sender);
        let mut got = Vec::new();
        while let Some(event) = receiver.recv().await {
            got.push(event);
        }
        assert_eq!(
            got,
            vec![
                StreamEvent::Reasoning("想".to_string()),
                StreamEvent::Text("好的".to_string()),
                StreamEvent::FunctionCall {
                    call_id: "c1".to_string(),
                    name: "generate_image".to_string(),
                    arguments: "{\"prompt\":\"x\"}".to_string(),
                },
            ],
            "the accumulated arguments win over the item's own copy"
        );
    }

    #[tokio::test]
    async fn a_failed_frame_ends_the_stream_and_a_blank_delta_is_dropped() {
        let (sender, mut receiver) = mpsc::channel(4);
        assert!(
            handle_frame(
                &Some("response.output_text.delta".to_string()),
                "{\"delta\":\"\"}",
                &mut None,
                &sender
            )
            .await
        );
        assert!(
            !handle_frame(
                &Some("response.failed".to_string()),
                "{\"error\":{\"message\":\"额度不足\"}}",
                &mut None,
                &sender
            )
            .await,
            "a failure stops the loop"
        );
        assert_eq!(
            receiver.recv().await,
            Some(StreamEvent::Failed("额度不足".to_string()))
        );
        // `try_recv`, not `recv().await`: the sender is still alive here, so
        // awaiting would park forever on an empty queue instead of answering the
        // question, which is only whether anything else was emitted.
        assert!(matches!(
            receiver.try_recv(),
            Err(TryRecvError::Empty),
        ));
    }

    #[tokio::test]
    async fn a_dropped_reader_cancels_the_pump() {
        let (sender, receiver) = mpsc::channel(1);
        drop(receiver);
        assert!(
            !handle_frame(
                &Some("response.output_text.delta".to_string()),
                "{\"delta\":\"x\"}",
                &mut None,
                &sender
            )
            .await,
            "sending into a gone reader ends the task"
        );
    }
}
