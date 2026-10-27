use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};

use crate::bailian::responses::FunctionTool;
use crate::net::ws::{encode, Frame, WsDecoder};

use super::realtime::{
    connect, message, RealtimeEvent, UpstreamError, UpstreamSocket, ACK_TIMEOUT,
};

/// The handle a call talks to upstream.
///
/// Channels instead of a trait object, so the state machine cannot tell the
/// difference between a TLS socket and a test fixture, and so the one ordering
/// rule that matters - every tool result acknowledged before the next response is
/// requested - lives in a single place.
pub enum Command {
    /// Send the session configuration. Emitted by the reader on
    /// `session.created`, because the endpoint accepts it no earlier.
    Configure(Value),
    AppendAudio(String),
    Commit,
    CreateResponse,
    /// A tool result. The sender resolves when the endpoint acknowledges the
    /// created item, or after `ACK_TIMEOUT`.
    ToolOutput {
        call_id: String,
        output: String,
        ack: oneshot::Sender<()>,
    },
    /// A ping from the endpoint, answered by whoever owns the write half.
    Pong(Vec<u8>),
    Close,
}

/// Outstanding tool-result acknowledgements, oldest first.
///
/// The writer enqueues when a result leaves, the reader resolves when the
/// endpoint says the item was created - the two ends of one ordering guarantee,
/// which is why they share this rather than each keeping their own list.
type AckQueue = std::sync::Arc<std::sync::Mutex<Vec<oneshot::Sender<()>>>>;

pub struct Upstream {
    pub commands: mpsc::Sender<Command>,
    pub events: mpsc::UnboundedReceiver<RealtimeEvent>,
}

impl Upstream {
    /// Hands a tool result back and waits for its acknowledgement.
    ///
    /// The wait is bounded on purpose: an endpoint that never acknowledges must
    /// not deadlock the call, and the Nest version had the same 3 second ceiling.
    pub async fn submit_tool_result(&self, call_id: &str, output: &str) {
        let (ack, received) = oneshot::channel();
        if self
            .commands
            .send(Command::ToolOutput {
                call_id: call_id.to_string(),
                output: output.to_string(),
                ack,
            })
            .await
            .is_err()
        {
            return;
        }
        tokio::time::timeout(ACK_TIMEOUT, received).await.ok();
    }
}

/// The frame for one command, as the upstream expects it.
pub fn command_frame(command: &Command) -> Frame {
    match command {
        Command::Configure(payload) => Frame::Text(payload.to_string()),
        Command::AppendAudio(pcm) => Frame::Text(message::append_audio(pcm).to_string()),
        Command::Commit => Frame::Text(message::commit().to_string()),
        Command::CreateResponse => Frame::Text(message::response_create().to_string()),
        Command::ToolOutput { call_id, output, .. } => {
            Frame::Text(message::function_call_output(call_id, output).to_string())
        }
        Command::Pong(payload) => Frame::Pong(payload.clone()),
        Command::Close => Frame::Close(Some(1000)),
    }
}

/// Dials the provider and runs the two loops over the split socket.
pub async fn open(
    base_url: &str,
    model: &str,
    api_key: &str,
    instructions: &str,
    tools: Vec<FunctionTool>,
) -> Result<Upstream, UpstreamError> {
    let UpstreamSocket { reader, writer } = connect(base_url, model, api_key).await?;
    let (command_tx, command_rx) = mpsc::channel::<Command>(64);
    let (event_tx, event_rx) = mpsc::unbounded_channel::<RealtimeEvent>();

    let setup = message::session_update(
        instructions,
        tools
            .into_iter()
            .filter_map(|tool| serde_json::to_value(tool).ok())
            .collect(),
    );

    let pending_acks: AckQueue = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    tokio::spawn(write_loop(writer, command_rx, std::sync::Arc::clone(&pending_acks)));
    tokio::spawn(read_loop(
        reader,
        event_tx,
        command_tx.clone(),
        setup,
        pending_acks,
    ));

    Ok(Upstream {
        commands: command_tx,
        events: event_rx,
    })
}

async fn write_loop<W: AsyncWriteExt + Unpin>(
    mut writer: W,
    mut commands: mpsc::Receiver<Command>,
    pending_acks: AckQueue,
) {
    while let Some(command) = commands.recv().await {
        let closing = matches!(command, Command::Close);
        let bytes = encode(&command_frame(&command), true);
        if writer.write_all(&bytes).await.is_err() {
            return;
        }
        // Only after the write succeeded: an ack entry must not exist for a
        // result that never reached the wire, or the next item's ack would be
        // spent on a submission the endpoint never saw.
        if let Command::ToolOutput { ack, .. } = command {
            pending_acks.lock().expect("ack queue").push(ack);
        }
        if closing {
            writer.shutdown().await.ok();
            return;
        }
    }
}

async fn read_loop<R: AsyncReadExt + Unpin>(
    mut reader: R,
    events: mpsc::UnboundedSender<RealtimeEvent>,
    commands: mpsc::Sender<Command>,
    setup: Value,
    pending_acks: AckQueue,
) {
    let mut stitch = super::realtime::UpstreamStitch::new();
    let mut decoder = WsDecoder::new();
    let mut configured = false;
    let mut buf = vec![0u8; 8 << 10];

    loop {
        let read = match reader.read(&mut buf).await {
            Ok(0) => {
                let _ = events.send(RealtimeEvent::Closed {
                    code: 1006,
                    reason: String::new(),
                });
                return;
            }
            Ok(read) => read,
            Err(error) => {
                let _ = events.send(RealtimeEvent::Failed(format!("上游读取失败: {error}")));
                return;
            }
        };
        let frames = match decoder.push(&buf[..read], false) {
            Ok(frames) => frames,
            Err(error) => {
                let _ = events.send(RealtimeEvent::Failed(format!("上游帧错误: {error:?}")));
                return;
            }
        };

        for frame in frames {
            match frame {
                Frame::Text(text) => {
                    // One pass per frame: the stitch accumulates deltas, so feeding
                    // the same text twice would double the transcript.
                    let produced = stitch.handle(&text);
                    if !configured
                        && produced
                            .iter()
                            .any(|event| *event == RealtimeEvent::SessionCreated)
                    {
                        configured = true;
                        if commands.send(Command::Configure(setup.clone())).await.is_err() {
                            return;
                        }
                    }
                    for event in produced {
                        match event {
                            RealtimeEvent::ItemCreated => {
                                let next = {
                                    let mut queue = pending_acks.lock().expect("ack queue");
                                    (!queue.is_empty()).then(|| queue.remove(0))
                                };
                                if let Some(ack) = next {
                                    let _ = ack.send(());
                                }
                            }
                            other => {
                                let _ = events.send(other);
                            }
                        }
                    }
                }
                Frame::Ping(payload) => {
                    if commands.send(Command::Pong(payload)).await.is_err() {
                        return;
                    }
                }
                Frame::Close(code) => {
                    let _ = events.send(RealtimeEvent::Closed {
                        code: code.unwrap_or(1000),
                        reason: String::new(),
                    });
                    return;
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{command_frame, Command, Frame};
    use serde_json::json;

    fn text_of(frame: &Frame) -> &str {
        match frame {
            Frame::Text(text) => text.as_str(),
            other => panic!("expected a text frame, got {other:?}"),
        }
    }

    #[test]
    fn each_command_becomes_the_payload_the_upstream_documents() {
        assert_eq!(
            text_of(&command_frame(&Command::AppendAudio("AAA=".into()))),
            json!({"type":"input_audio_buffer.append","audio":"AAA="}).to_string()
        );
        assert_eq!(
            text_of(&command_frame(&Command::Commit)),
            json!({"type":"input_audio_buffer.commit"}).to_string()
        );
        assert_eq!(
            text_of(&command_frame(&Command::CreateResponse)),
            json!({"type":"response.create"}).to_string()
        );
        assert_eq!(
            text_of(&command_frame(&Command::ToolOutput {
                call_id: "c1".into(),
                output: "asset 7".into(),
                ack: tokio::sync::oneshot::channel().0,
            })),
            json!({"type":"conversation.item.create","item":{"type":"function_call_output","call_id":"c1","output":"asset 7"}})
                .to_string()
        );
        assert_eq!(
            text_of(&command_frame(&Command::Configure(json!({"type": "session.update"})))),
            r#"{"type":"session.update"}"#
        );
    }

    #[test]
    fn a_close_command_is_a_close_frame_with_a_normal_code() {
        assert_eq!(command_frame(&Command::Close), Frame::Close(Some(1000)));
        assert_eq!(command_frame(&Command::Pong(vec![1, 2])), Frame::Pong(vec![1, 2]));
    }
}
