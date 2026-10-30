use futures_util::StreamExt;

use crate::bailian::responses::{RequestItem, ResponsesClient, StreamEvent, StreamRequest};
use crate::common::args::parse_tool_arguments;
use crate::conversation::{asset_url, ConversationEvent, ToolCallRecord};
use crate::state::AppState;

use super::mode::{Mode, ModePrompt};
use super::tools::{describe_outcome, ToolContext, ToolRegistry};

/// Guards against a tool loop that never converges.
const MAX_TOOL_ROUNDS: usize = 5;

struct ParsedCall {
    call_id: String,
    name: String,
    arguments: String,
}

/// One turn's accumulated output, kept together so every exit path finalizes the
/// same way.
#[derive(Default)]
struct Turn {
    reasoning: String,
    answer: String,
    tool_records: Vec<ToolCallRecord>,
    failure: Option<String>,
}

pub struct AgentService {
    state: AppState,
}

impl AgentService {
    pub fn new(state: AppState) -> Self {
        Self { state }
    }

    /// Executes one agent turn for a conversation. The rows the turn writes into
    /// (the user message, the assistant placeholder) already exist: the caller
    /// (`TurnRunner`) created them synchronously before it returned control, so
    /// submitting and running are decoupled from any HTTP connection — which is
    /// what lets a turn outlive a dropped client.
    ///
    /// Everything the browser sees goes through the conversation's event journal,
    /// not a response stream: each event gets a `seq` and is replayable after a
    /// reconnect. Never fails — the journal outcome is `turn_done` or `turn_error`,
    /// and the placeholder is always finalized.
    pub async fn run(&self, conversation_id: i64, message_id: i64, mode: Mode) {
        let state = self.state.clone();
        let mut turn = Turn::default();

        let mut items: Vec<RequestItem> = match state
            .conversations
            .build_input_items(conversation_id)
            .await
        {
            Ok(history) => history.into_iter().map(RequestItem::Message).collect(),
            Err(error) => {
                turn.failure = Some(error.describe());
                self.finalize(conversation_id, message_id, &turn).await;
                return;
            }
        };

        let api_key = match state.require_key().await {
            Ok(key) => key,
            Err(error) => {
                turn.failure = Some(error.describe());
                self.finalize(conversation_id, message_id, &turn).await;
                return;
            }
        };

        for _round in 0..MAX_TOOL_ROUNDS {
            let calls = match self
                .run_round(&state, &api_key, conversation_id, message_id, mode, &mut items, &mut turn)
                .await
            {
                Ok(calls) => calls,
                Err(message) => {
                    turn.failure = Some(message);
                    break;
                }
            };
            if calls.is_empty() {
                break;
            }
            for call in calls {
                self.run_tool(&state, conversation_id, message_id, &call, &mut items, &mut turn)
                    .await;
            }
        }

        self.finalize(conversation_id, message_id, &turn).await;
    }

    /// One LLM round trip: stream deltas into the journal and collect the tool
    /// calls the model asked for. The history is snapshotted per round because
    /// this loop is stateless — there is no `previous_response_id`.
    async fn run_round(
        &self,
        state: &AppState,
        api_key: &str,
        conversation_id: i64,
        message_id: i64,
        mode: Mode,
        items: &mut Vec<RequestItem>,
        turn: &mut Turn,
    ) -> Result<Vec<ParsedCall>, String> {
        let instructions = mode.build_instructions();
        let mut stream = ResponsesClient::stream(
            &state.http,
            &state.config,
            api_key,
            StreamRequest {
                input: items.clone(),
                tools: ToolRegistry::specs_for(mode),
                instructions: Some(&instructions),
            },
        )
        .await
        .map_err(|error| error.describe())?;

        let mut calls: Vec<ParsedCall> = Vec::new();
        while let Some(event) = stream.next().await {
            match event {
                StreamEvent::Reasoning(delta) => {
                    turn.reasoning.push_str(&delta);
                    self.emit(state, conversation_id, ConversationEvent::Reasoning { message_id, delta })
                        .await;
                }
                StreamEvent::Text(delta) => {
                    turn.answer.push_str(&delta);
                    self.emit(state, conversation_id, ConversationEvent::Text { message_id, delta })
                        .await;
                }
                StreamEvent::FunctionCall { call_id, name, arguments } => {
                    calls.push(ParsedCall { call_id, name, arguments });
                }
                StreamEvent::Failed(message) => return Err(message),
            }
        }
        Ok(calls)
    }

    /// Runs one tool call, journals its result, persists any assets it produced
    /// and appends the round trip to the conversation for the next round.
    async fn run_tool(
        &self,
        state: &AppState,
        conversation_id: i64,
        message_id: i64,
        call: &ParsedCall,
        items: &mut Vec<RequestItem>,
        turn: &mut Turn,
    ) {
        let arguments = parse_tool_arguments(&call.arguments);
        self.emit(
            state,
            conversation_id,
            ConversationEvent::ToolCall {
                message_id,
                name: call.name.clone(),
                arguments: arguments.clone(),
                status: "running".to_string(),
            },
        )
        .await;

        let outcome = ToolRegistry::new(state.clone())
            .execute(
                &call.name,
                &arguments,
                ToolContext { conversation_id, message_id },
            )
            .await;

        let mut asset_ids: Vec<i64> = Vec::new();
        for asset in &outcome.assets {
            match state.conversations.add_asset(message_id, asset).await {
                Ok(asset_id) => {
                    asset_ids.push(asset_id);
                    self.emit(
                        state,
                        conversation_id,
                        ConversationEvent::Asset {
                            message_id,
                            id: asset_id,
                            kind: asset.kind,
                            url: asset_url(asset_id),
                            mime: asset.mime.clone(),
                            bytes: asset.bytes,
                        },
                    )
                    .await;
                }
                Err(error) => tracing::warn!(%error, "asset row could not be written"),
            }
        }

        let summary = describe_outcome(&outcome, &asset_ids);
        self.emit(
            state,
            conversation_id,
            ConversationEvent::ToolResult {
                message_id,
                name: call.name.clone(),
                ok: outcome.ok,
                summary: summary.clone(),
            },
        )
        .await;
        turn.tool_records.push(ToolCallRecord {
            name: call.name.clone(),
            arguments,
            ok: outcome.ok,
            summary: summary.clone(),
        });

        // The call must be echoed back before its result.
        items.push(RequestItem::call(&call.call_id, &call.name, &call.arguments));
        items.push(RequestItem::output(&call.call_id, &summary));
    }

    /// Writes the assistant row and journals the turn's last event, in that order.
    ///
    /// A failed turn must still leave a reply behind: `build_input_items` skips
    /// empty messages, so an empty assistant row leaves its user message orphaned —
    /// and the next turn replays two user messages back to back, which makes the
    /// model re-run the failed request as if it were new. Measured: that duplicated
    /// a paid video submission. This finalization is the load-bearing guarantee.
    async fn finalize(&self, conversation_id: i64, message_id: i64, turn: &Turn) {
        let state = self.state.clone();
        let content = match turn.failure.as_deref() {
            None => turn.answer.clone(),
            Some(reason) => {
                [turn.answer.clone(), format!("本轮处理失败：{reason}")]
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n\n")
            }
        };
        let records = (!turn.tool_records.is_empty()).then_some(turn.tool_records.as_slice());

        if let Err(error) = state
            .conversations
            .finalize_assistant(
                message_id,
                &content,
                (!turn.reasoning.is_empty()).then_some(turn.reasoning.as_str()),
                records,
            )
            .await
        {
            tracing::warn!(%error, "assistant row could not be finalized");
        }
        if let Err(error) = state.conversations.touch(conversation_id).await {
            tracing::warn!(%error, "conversation could not be touched");
        }

        // Journaled after the rows are durable: a client that reconnects past this
        // event and resyncs reads the finalized content, never the placeholder.
        let event = match turn.failure.as_deref() {
            None => ConversationEvent::TurnDone { message_id },
            Some(reason) => ConversationEvent::TurnError {
                message_id,
                message: reason.to_string(),
            },
        };
        self.emit(&state, conversation_id, event).await;
    }

    async fn emit(&self, state: &AppState, conversation_id: i64, event: ConversationEvent) {
        if let Err(error) = state.events.publish(conversation_id, event).await {
            tracing::warn!(%error, "event could not be journaled");
        }
    }
}
