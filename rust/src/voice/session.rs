use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::Sleep;

use crate::agent::tools::{describe_outcome, ToolContext, ToolRegistry};
use crate::common::args::parse_tool_arguments;
use crate::conversation::{asset_url, ToolCallRecord};
use crate::state::AppState;

use super::protocol::{ClientFrame, ServerFrame};
use super::realtime::{RealtimeEvent, UpstreamError};
use super::relay::{Inbound, Outbound};
use super::upstream::{Command, Upstream};

/// Bounds a tool loop that never converges, as `MAX_TOOL_ROUNDS` does for text.
const MAX_TOOL_ROUNDS: usize = 5;

/// How long to wait for a transcript before assuming the utterance was silent.
const SILENCE_TIMEOUT: Duration = Duration::from_secs(12);

/// Instructions for a spoken assistant - written for speech, not for a
/// transcript: no markdown, no URLs, short replies.
const VOICE_INSTRUCTIONS: &str = concat!(
    "你是一个生成式媒体助手，正在通过语音与用户实时对话。\n",
    "\n",
    "规则：\n",
    "- 用中文口语回答，简短自然，一次说一两句。\n",
    "- 生成的图片或视频会自动显示在用户屏幕上，不要念出链接，也不要描述文件格式。\n",
    "- 开始生成前先用一句话说明你要画什么。\n",
);

/// Shown in history when a turn produced neither speech nor a tool call.
const EMPTY_TURN_NOTE: &str = "（本轮没有语音输出）";

/// One live call: the per-socket state machine.
///
/// A voice turn is *not* replayed to the model the way a text turn is - the
/// upstream session keeps its own context for the life of the call - so what is
/// written down here is history and UI, not model input.
pub struct VoiceCall {
    state: AppState,
    user_id: i64,
    out: mpsc::Sender<Outbound>,
    upstream: Option<Upstream>,
    conversation_id: Option<i64>,
    assistant_message_id: Option<i64>,
    assistant_text: String,
    tool_calls: Vec<ToolCallRecord>,
    /// Calls collected during the current response, dispatched on `response.done`:
    /// one response can carry several, and acting on the first would request the
    /// next response before the rest had been answered.
    pending_calls: Vec<(String, String, String)>,
    tool_rounds: usize,
    audio_frames: usize,
    failures: usize,
    refused: bool,
    busy: bool,
    closed: bool,
    silence: Option<std::pin::Pin<Box<Sleep>>>,
}

impl VoiceCall {
    pub fn new(state: AppState, user_id: i64, out: mpsc::Sender<Outbound>) -> Self {
        Self {
            state,
            user_id,
            out,
            upstream: None,
            conversation_id: None,
            assistant_message_id: None,
            assistant_text: String::new(),
            tool_calls: Vec::new(),
            pending_calls: Vec::new(),
            tool_rounds: 0,
            audio_frames: 0,
            failures: 0,
            refused: false,
            busy: false,
            closed: false,
            silence: None,
        }
    }

    /// Drives the call until the browser socket ends.
    pub async fn run(mut self, mut inbound: mpsc::Receiver<Inbound>) {
        loop {
            tokio::select! {
                next = inbound.recv() => match next {
                    None | Some(Inbound::Closed(_)) => break,
                    Some(Inbound::Protocol(message)) => {
                        tracing::warn!(%message, "voice socket broke protocol");
                        break;
                    }
                    Some(Inbound::Ping(payload)) => {
                        let _ = self.out.send(Outbound::Pong(payload)).await;
                    }
                    Some(Inbound::Frame(frame)) => self.receive(frame).await,
                },
                event = Self::next_event(self.upstream.as_mut()) => {
                    if let Some(event) = event {
                        self.on_realtime(event).await;
                    } else {
                        // No upstream yet: wait on the browser only rather than
                        // spinning on a closed channel.
                        tokio::time::sleep(Duration::from_millis(25)).await;
                    }
                },
                _ = Self::fire_silence(&mut self.silence), if self.silence.is_some() => {
                    self.silence = None;
                    self.silence_guard().await;
                }
            }
        }
        self.close().await;
    }

    async fn next_event(upstream: Option<&mut Upstream>) -> Option<RealtimeEvent> {
        match upstream {
            None => std::future::pending::<Option<RealtimeEvent>>().await,
            Some(handle) => handle.events.recv().await,
        }
    }

    /// A placeholder branch when no guard is armed, so `select!` never completes
    /// this arm by itself.
    async fn fire_silence(slot: &mut Option<std::pin::Pin<Box<Sleep>>>) {
        match slot {
            Some(sleep) => sleep.as_mut().await,
            None => std::future::pending::<()>().await,
        }
    }

    /// The dial, factored out so a test can hand in a fixture instead.
    pub fn attach_upstream(&mut self, upstream: Upstream) {
        self.upstream = Some(upstream);
    }

    pub fn conversation_id(&self) -> Option<i64> {
        self.conversation_id
    }

    async fn receive(&mut self, frame: ClientFrame) {
        match frame {
            ClientFrame::Start => self.start().await,
            ClientFrame::Audio { pcm } => {
                self.audio_frames += 1;
                // Once the endpoint has refused this utterance, appending more of
                // it only earns more refusals. The next commit starts fresh.
                if !self.refused {
                    self.command(Command::AppendAudio(pcm)).await;
                }
            }
            ClientFrame::Commit => {
                if self.busy {
                    tracing::warn!("ignoring an utterance while a turn is still running");
                    return;
                }
                self.busy = true;
                self.refused = false;
                self.command(Command::Commit).await;
                // The frame count is what tells "the microphone sent nothing" from
                // "the endpoint rejected what it sent"; both look the same to the
                // browser and both end in an error line.
                tracing::info!(frames = self.audio_frames, "voice turn committed");
                self.audio_frames = 0;
                self.arm_silence_guard();
            }
            ClientFrame::Stop => self.close().await,
        }
    }

    async fn start(&mut self) {
        if self.upstream.is_some() {
            return;
        }
        let conversation_id = match self.state.conversations.create(self.user_id, "语音会话").await {
            Ok(id) => id,
            Err(error) => {
                tracing::warn!(%error, "the voice conversation could not be opened");
                return;
            }
        };
        self.conversation_id = Some(conversation_id);

        let api_key = match self.state.require_key().await {
            Ok(key) => key,
            Err(error) => return self.fail(error.describe()).await,
        };
        match super::upstream::open(
            &self.state.config.bailian.base_url,
            &self.state.config.bailian.voice_model,
            &api_key,
            VOICE_INSTRUCTIONS,
            ToolRegistry::all_specs(),
        )
        .await
        {
            Ok(handle) => {
                self.upstream = Some(handle);
                // Both ends of the call are logged: the incident this answers was
                // a client that never sent `start` while the server said nothing.
                tracing::info!(
                    conversation = conversation_id,
                    user = self.user_id,
                    "voice session opened"
                );
            }
            Err(error) => {
                let message = match error {
                    UpstreamError::Rejected(text) => text,
                    UpstreamError::Malformed(text) => text,
                    UpstreamError::BadAccept => "语音服务拒绝了握手".to_string(),
                };
                self.fail(message).await;
            }
        }
    }

    async fn fail(&mut self, message: String) {
        tracing::warn!(%message, "voice session failed");
        self.send(ServerFrame::Error { message }).await;
        self.close().await;
    }

    async fn on_realtime(&mut self, event: RealtimeEvent) {
        match event {
            RealtimeEvent::SessionCreated | RealtimeEvent::ItemCreated => {}
            RealtimeEvent::Ready => {
                if let Some(conversation_id) = self.conversation_id {
                    self.send(ServerFrame::Ready { conversation_id }).await;
                }
            }
            RealtimeEvent::Transcript(text) => {
                self.open_turn(&text).await;
                // Push-to-talk does not auto-answer: with `turn_detection: null`
                // the server commits the audio and waits to be asked.
                self.command(Command::CreateResponse).await;
            }
            RealtimeEvent::AssistantText(text) => {
                // Trimmed per segment: the upstream transcript carries a trailing
                // newline, and joining those verbatim leaves blank lines in history.
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    return;
                }
                if !self.assistant_text.is_empty() {
                    self.assistant_text.push('\n');
                }
                self.assistant_text.push_str(trimmed);
                self.send(ServerFrame::AssistantText { text: trimmed.to_string() })
                    .await;
            }
            RealtimeEvent::Audio(pcm) => self.send(ServerFrame::Audio { pcm }).await,
            RealtimeEvent::FunctionCall { call_id, name, arguments } => {
                self.pending_calls.push((call_id, name.clone(), arguments));
                self.send(ServerFrame::ToolCall { name, status: "running".to_string() })
                    .await;
            }
            RealtimeEvent::ResponseDone => {
                if self.pending_calls.is_empty() {
                    self.finalize_turn().await;
                } else {
                    self.dispatch_pending().await;
                }
            }
            RealtimeEvent::Failed(message) => {
                self.failures += 1;
                self.refused = true;
                tracing::warn!(%message, failures = self.failures, "voice turn failed");
                // One line in the UI per turn: a run of failures is one problem,
                // and the page can only show so many.
                if self.failures == 1 {
                    self.send(ServerFrame::Error { message }).await;
                }
                // The turn is over even though it failed; leaving it open would
                // hold the button disabled for the rest of the call.
                self.finalize_turn().await;
            }
            RealtimeEvent::Closed { code, reason } => {
                if code != 1000 && !self.closed {
                    tracing::warn!(code, %reason, "voice session closed by the endpoint");
                }
                self.close().await;
            }
        }
    }

    /// An utterance too quiet to transcribe produces no transcript, so no turn
    /// opens and the button would stay held for the rest of the call.
    fn arm_silence_guard(&mut self) {
        self.silence = Some(Box::pin(tokio::time::sleep(SILENCE_TIMEOUT)));
    }

    async fn silence_guard(&mut self) {
        if self.busy && self.assistant_message_id.is_none() {
            self.busy = false;
            tracing::warn!("voice turn produced no transcript (silence guard)");
            self.send(ServerFrame::Error {
                message: "没有听清，请再说一次".to_string(),
            })
            .await;
        }
    }

    async fn open_turn(&mut self, transcript: &str) {
        self.silence = None;
        self.failures = 0;
        tracing::info!(transcript = transcript.trim(), "voice transcript");
        let Some(conversation_id) = self.conversation_id else {
            return;
        };
        // `mode` is null: a voice turn runs in none of the four text modes
        // (docs/architecture.md §11).
        self.state
            .conversations
            .append_user_message(conversation_id, transcript, None)
            .await
            .ok();
        self.assistant_message_id = self
            .state
            .conversations
            .create_assistant_placeholder(conversation_id, None)
            .await
            .ok();
        self.assistant_text = String::new();
        self.tool_calls = Vec::new();
        self.tool_rounds = 0;
        self.send(ServerFrame::Transcript { text: transcript.to_string() })
            .await;
    }

    async fn dispatch_pending(&mut self) {
        let (Some(conversation_id), Some(message_id)) =
            (self.conversation_id, self.assistant_message_id)
        else {
            self.pending_calls.clear();
            return;
        };
        let calls = std::mem::take(&mut self.pending_calls);
        self.tool_rounds += 1;
        let registry = ToolRegistry::new(self.state.clone());
        let mut results = Vec::with_capacity(calls.len());

        for (call_id, name, arguments) in calls {
            let parsed = parse_tool_arguments(&arguments);
            let outcome = registry
                .execute(&name, &parsed, ToolContext { conversation_id, message_id })
                .await;

            let mut asset_ids = Vec::new();
            for asset in &outcome.assets {
                if let Ok(asset_id) = self.state.conversations.add_asset(message_id, asset).await {
                    asset_ids.push(asset_id);
                    self.send(ServerFrame::Asset {
                        id: asset_id,
                        kind: asset.kind,
                        url: asset_url(asset_id),
                        mime: asset.mime.clone(),
                        bytes: asset.bytes,
                    })
                    .await;
                }
            }

            // The ids ride back to the model: they are what lets a later call name
            // this image as the first frame of a video.
            let summary = describe_outcome(&outcome, &asset_ids);
            self.tool_calls.push(ToolCallRecord {
                name: name.clone(),
                arguments: parsed,
                ok: outcome.ok,
                summary: summary.clone(),
            });
            self.send(ServerFrame::ToolCall {
                name,
                status: if outcome.ok { "done" } else { "failed" }.to_string(),
            })
            .await;
            results.push((call_id, summary));
        }

        // Every result goes back, each waiting for its acknowledgement, before
        // the continuation is asked for.
        if let Some(upstream) = self.upstream.as_ref() {
            for (call_id, output) in &results {
                upstream.submit_tool_result(call_id, output).await;
            }
        }

        if self.tool_rounds >= MAX_TOOL_ROUNDS {
            tracing::warn!(cap = MAX_TOOL_ROUNDS, "voice turn hit the tool round cap");
            self.send(ServerFrame::Error {
                message: "工具调用轮次过多，已停止".to_string(),
            })
            .await;
            self.finalize_turn().await;
            return;
        }
        self.command(Command::CreateResponse).await;
    }

    async fn finalize_turn(&mut self) {
        self.busy = false;
        self.silence = None;
        let (Some(message_id), Some(conversation_id)) =
            (self.assistant_message_id, self.conversation_id)
        else {
            return;
        };
        self.assistant_message_id = None;

        // Speech if there was any, otherwise the tool trail stands alone; the note
        // is only for a turn that produced neither.
        let content = if !self.assistant_text.is_empty() {
            self.assistant_text.clone()
        } else if !self.tool_calls.is_empty() {
            String::new()
        } else {
            EMPTY_TURN_NOTE.to_string()
        };
        let records = (!self.tool_calls.is_empty()).then_some(self.tool_calls.as_slice());
        self.state
            .conversations
            .finalize_assistant(message_id, &content, None, records)
            .await
            .ok();
        self.state.conversations.touch(conversation_id).await.ok();
        self.send(ServerFrame::Done).await;
    }

    async fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        tracing::info!(
            conversation = self
                .conversation_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "未建立".to_string()),
            "voice call ended"
        );
        if let Some(upstream) = self.upstream.as_ref() {
            let _ = upstream.commands.send(Command::Close).await;
        }
        self.upstream = None;
        // Never leave an unfinalised assistant row: an orphaned placeholder is what
        // made a later turn replay two user messages back to back in the text path.
        self.finalize_turn().await;
        let _ = self.out.send(Outbound::Close(1000)).await;
    }

    async fn command(&self, command: Command) {
        if let Some(upstream) = self.upstream.as_ref() {
            let _ = upstream.commands.send(command).await;
        }
    }

    async fn send(&self, frame: ServerFrame) {
        if self.closed {
            return;
        }
        let _ = self.out.clone().send(Outbound::Frame(frame)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
    use tokio::sync::mpsc;

    /// A call with a fixture upstream: commands land on a channel the test reads,
    /// events come from one the test writes. No socket, no provider.
    async fn fixture() -> (
        VoiceCall,
        mpsc::Receiver<Command>,
        mpsc::UnboundedSender<RealtimeEvent>,
        mpsc::Receiver<Outbound>,
        AppState,
    ) {
        let state = AppState::new(
            AppConfig::test("/tmp/kestrel-voice-tests", "https://try.kestrel.test"),
            crate::db::open_in_memory().await,
            Some("test-key".to_string()),
        );
        let (cmd_tx, cmd_rx) = mpsc::channel(16);
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (out_tx, out_rx) = mpsc::channel(64);
        // A real account row: `conversations.user_id` has a foreign key, and the
        // point of these tests is that the rows land in the same database the text
        // path writes.
        sqlx::query("INSERT INTO users (id, username, password_hash, role, created_at) VALUES (1, 'walker', 'h', 'user', 1)")
            .execute(&state.pool)
            .await
            .expect("user");
        let mut call = VoiceCall::new(state.clone(), 1, out_tx);
        call.attach_upstream(Upstream::from_channels(cmd_tx, event_rx));
        call.conversation_id = Some(state.conversations.create(1, "语音会话").await.expect("conversation"));
        let expected = call.conversation_id;
        assert!(expected.is_some());
        (call, cmd_rx, event_tx, out_rx, state)
    }

    fn frames(out: &mut mpsc::Receiver<Outbound>) -> Vec<String> {
        let mut got = Vec::new();
        while let Ok(item) = out.try_recv() {
            match item {
                Outbound::Frame(frame) => got.push(frame.to_json()),
                Outbound::Pong(_) => got.push("pong".to_string()),
                Outbound::Close(code) => got.push(format!("close:{code}")),
            }
        }
        got
    }

    #[tokio::test]
    async fn a_transcript_opens_the_turn_persists_both_rows_and_asks_for_a_response() {
        let (mut call, mut commands, _events, mut out, state) = fixture().await;
        call.on_realtime(RealtimeEvent::Transcript("画一只红隼".to_string()))
            .await;

        let conversation_id = call.conversation_id.expect("conversation");
        let views = state.conversations.views(conversation_id).await.expect("views");
        assert_eq!(views.len(), 2, "user row plus the assistant placeholder");
        assert_eq!(views[0].content, "画一只红隼");
        assert_eq!(views[0].mode, None, "a voice turn has no mode");
        assert!(views[1].content.is_empty(), "the placeholder is not painted yet");
        call.assistant_message_id = Some(views[1].id);

        assert!(
            frames(&mut out).iter().any(|frame| frame.contains("\"type\":\"transcript\"")),
            "the browser is told what was heard"
        );
        match commands.recv().await.expect("a command") {
            Command::CreateResponse => {}
            other => panic!("push-to-talk must be answered on request, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn audio_stops_flowing_after_the_endpoint_refuses_this_utterance() {
        let (mut call, mut commands, _events, mut out, _state) = fixture().await;

        call.receive(ClientFrame::Audio { pcm: "AAA=".into() }).await;
        assert!(matches!(commands.recv().await, Some(Command::AppendAudio(_))));

        call.on_realtime(RealtimeEvent::Failed("buffer overflow".into())).await;
        call.receive(ClientFrame::Audio { pcm: "BBB=".into() }).await;
        // Refusing does not reset the count; only a commit does, because the number
        // is what distinguishes an empty microphone from a rejected buffer for the
        // utterance that is still being described.
        assert_eq!(call.audio_frames, 2, "the count keeps across a refusal");
        call.receive(ClientFrame::Commit).await;
        assert_eq!(call.audio_frames, 0, "the next utterance starts from zero");
        let mut queued = Vec::new();
        while let Ok(command) = commands.try_recv() {
            queued.push(command);
        }
        assert!(
            !queued
                .iter()
                .any(|command| matches!(command, Command::AppendAudio(_))),
            "a refused utterance must not keep feeding audio: {queued:?}"
        );

        // One error line per turn, however many refusals follow it.
        let first = frames(&mut out);
        assert_eq!(
            first.iter().filter(|frame| frame.contains("\"type\":\"error\"")).count(),
            1,
            "{first:?}"
        );
        call.on_realtime(RealtimeEvent::Failed("buffer overflow".into())).await;
        assert!(frames(&mut out).is_empty(), "a run of failures is one problem");
    }

    #[tokio::test]
    async fn a_failed_turn_still_finalises_so_the_button_is_not_held_forever() {
        let (mut call, _commands, _events, mut out, state) = fixture().await;
        let conversation_id = call.conversation_id.expect("conversation");
        call.assistant_message_id = Some(
            state
                .conversations
                .create_assistant_placeholder(conversation_id, None)
                .await
                .expect("placeholder"),
        );
        call.busy = true;

        call.on_realtime(RealtimeEvent::Failed("额度不足".into())).await;

        assert!(!call.busy, "the turn is over even though it failed");
        assert!(call.assistant_message_id.is_none());
        let views = state.conversations.views(conversation_id).await.expect("views");
        assert_eq!(
            views.last().expect("assistant").content,
            EMPTY_TURN_NOTE,
            "an empty placeholder would orphan the user row"
        );
        assert!(frames(&mut out).iter().any(|frame| frame.contains("\"type\":\"done\"")));
    }

    #[tokio::test]
    async fn speech_is_joined_from_trimmed_segments() {
        let (mut call, _commands, _events, mut out, state) = fixture().await;
        let conversation_id = call.conversation_id.expect("conversation");
        call.assistant_message_id = Some(
            state
                .conversations
                .create_assistant_placeholder(conversation_id, None)
                .await
                .expect("placeholder"),
        );

        for segment in ["第一句\n", "  ", "第二句"] {
            call.on_realtime(RealtimeEvent::AssistantText(segment.to_string())).await;
        }
        assert_eq!(call.assistant_text, "第一句\n第二句", "blank segments add nothing");
        call.finalize_turn().await;
        let views = state.conversations.views(conversation_id).await.expect("views");
        assert_eq!(views.last().expect("assistant").content, "第一句\n第二句");
        assert_eq!(
            frames(&mut out)
                .iter()
                .filter(|frame| frame.contains("\"type\":\"assistant_text\""))
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn a_second_commit_while_busy_is_dropped_not_queued() {
        let (mut call, mut commands, _events, _out, _state) = fixture().await;
        call.receive(ClientFrame::Commit).await;
        assert!(matches!(commands.recv().await, Some(Command::Commit)));
        assert!(call.busy);

        call.receive(ClientFrame::Commit).await;
        assert!(commands.try_recv().is_err(), "the UI disables the button; a stray press is dropped");
    }

    #[tokio::test]
    async fn the_silence_guard_releases_a_turn_that_never_transcribed() {
        let (mut call, _commands, _events, mut out, _state) = fixture().await;
        call.busy = true;
        call.assistant_message_id = None;
        call.silence_guard().await;
        assert!(!call.busy);
        let sent = frames(&mut out);
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert!(sent[0].contains("没有听清，请再说一次"), "{}", sent[0]);

        // A turn that did open has rows to finalise instead, so the guard is quiet.
        call.busy = true;
        call.assistant_message_id = Some(1);
        call.silence_guard().await;
        assert!(call.busy, "not our business");
        assert!(frames(&mut out).is_empty());
    }

    #[tokio::test]
    async fn close_finalises_and_takes_the_upstream_with_it() {
        let (mut call, mut commands, _events, mut out, state) = fixture().await;
        let conversation_id = call.conversation_id.expect("conversation");
        call.assistant_message_id = Some(
            state
                .conversations
                .create_assistant_placeholder(conversation_id, None)
                .await
                .expect("placeholder"),
        );

        call.close().await;
        assert!(call.closed);
        assert!(state
            .conversations
            .unfinished_assistant(conversation_id)
            .await
            .expect("probe")
            .is_none());
        assert!(matches!(commands.recv().await, Some(Command::Close)));
        let sent = frames(&mut out);
        // `close()` sets the flag first, so the turn's `done` is suppressed - the
        // same guard the Nest `send()` has once the socket is being torn down. What
        // must still go out is the close frame itself.
        assert_eq!(sent, vec!["close:1000".to_string()], "{sent:?}");
    }

    #[tokio::test]
    async fn a_ready_frame_needs_a_conversation_to_report() {
        let (mut call, _commands, _events, mut out, _state) = fixture().await;
        call.on_realtime(RealtimeEvent::Ready).await;
        assert_eq!(frames(&mut out).len(), 1);

        let state = AppState::new(
            AppConfig::test("/tmp/kestrel-voice-tests", "https://try.kestrel.test"),
            crate::db::open_in_memory().await,
            Some("k".to_string()),
        );
        let (out_tx, mut out_rx) = mpsc::channel(4);
        let mut bare = VoiceCall::new(state, 1, out_tx);
        bare.on_realtime(RealtimeEvent::Ready).await;
        assert!(out_rx.try_recv().is_err(), "nothing to report yet");
        bare.close().await;
    }

    /// Reads commands as they arrive, answering the way the endpoint would: a tool
    /// result gets its acknowledgement immediately, so the dispatch path is not
    /// waiting on its `ACK_TIMEOUT` to prove the ordering.
    fn commands_in_order(mut rx: mpsc::Receiver<Command>) -> mpsc::UnboundedReceiver<String> {
        let (tx, labels) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Some(command) = rx.recv().await {
                let label = match command {
                    Command::ToolOutput { ack, .. } => {
                        let _ = ack.send(());
                        "tool_output"
                    }
                    Command::CreateResponse => "create_response",
                    Command::Commit => "commit",
                    Command::AppendAudio(_) => "append_audio",
                    Command::Configure(_) => "configure",
                    Command::Pong(_) => "pong",
                    Command::Close => "close",
                };
                let _ = tx.send(label.to_string());
            }
        });
        labels
    }

    /// Waits for the next command label, so the assertions cannot win or lose a
    /// race against the resolver task forwarding them.
    async fn next_label(rx: &mut mpsc::UnboundedReceiver<String>) -> String {
        tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| "<none>".to_string())
    }

    /// The quiet check: nothing further should arrive.
    async fn no_more_labels(rx: &mut mpsc::UnboundedReceiver<String>) -> bool {
        tokio::time::timeout(Duration::from_millis(150), rx.recv())
            .await
            .is_err()
    }

    /// Drives a tool round with a name the registry does not know, which is the
    /// only round that needs no provider. Everything else about the round - the
    /// running/done frames, the acknowledgement before the next response, the
    /// persisted trail - is the part worth pinning.
    #[tokio::test]
    async fn a_tool_round_asks_before_the_next_response_and_keeps_the_trail() {
        let (mut call, commands, _events, mut out, state) = fixture().await;
        let mut labels = commands_in_order(commands);
        let conversation_id = call.conversation_id.expect("conversation");
        call.assistant_message_id = Some(
            state
                .conversations
                .create_assistant_placeholder(conversation_id, None)
                .await
                .expect("placeholder"),
        );

        call.on_realtime(RealtimeEvent::FunctionCall {
            call_id: "c1".into(),
            name: "no_such_tool".into(),
            arguments: "{\"prompt\":\"红隼\"}".into(),
        })
        .await;
        assert!(
            frames(&mut out)
                .iter()
                .any(|frame| frame.contains("\"status\":\"running\"")),
            "the browser sees the call start"
        );

        call.on_realtime(RealtimeEvent::ResponseDone).await;

        let first = next_label(&mut labels).await;
        let second = next_label(&mut labels).await;
        assert_eq!(
            (first.as_str(), second.as_str()),
            ("tool_output", "create_response"),
            "the result goes back and only then is the next response asked for"
        );
        assert!(
            frames(&mut out)
                .iter()
                .any(|frame| frame.contains("\"status\":\"failed\"")),
            "an unknown tool is a failed result, not a silent one"
        );

        // A response with nothing pending now closes the turn, and the trail is
        // written down even though the assistant said nothing.
        call.on_realtime(RealtimeEvent::ResponseDone).await;
        let views = state.conversations.views(conversation_id).await.expect("views");
        let assistant = views.last().expect("assistant");
        assert_eq!(assistant.content, "", "a tools-only turn has no speech to store");
        let records = assistant.tool_calls.as_ref().expect("trail");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].name, "no_such_tool");
        assert!(!records[0].ok);
        assert!(
            records[0].summary.contains("Unknown tool"),
            "{}",
            records[0].summary
        );
    }

    #[tokio::test]
    async fn the_round_cap_stops_a_loop_that_never_converges() {
        let (mut call, commands, _events, mut out, state) = fixture().await;
        let mut labels = commands_in_order(commands);
        let conversation_id = call.conversation_id.expect("conversation");
        call.assistant_message_id = Some(
            state
                .conversations
                .create_assistant_placeholder(conversation_id, None)
                .await
                .expect("placeholder"),
        );

        let mut asked = 0usize;
        for round in 0..MAX_TOOL_ROUNDS {
            call.on_realtime(RealtimeEvent::FunctionCall {
                call_id: format!("c{round}"),
                name: "no_such_tool".into(),
                arguments: "{}".into(),
            })
            .await;
            call.on_realtime(RealtimeEvent::ResponseDone).await;
            assert_eq!(next_label(&mut labels).await, "tool_output", "round {round}");
            if round + 1 < MAX_TOOL_ROUNDS {
                assert_eq!(next_label(&mut labels).await, "create_response", "round {round}");
                asked += 1;
            } else {
                assert!(
                    no_more_labels(&mut labels).await,
                    "the capped round must not ask for another response"
                );
            }
        }

        assert_eq!(call.tool_rounds, MAX_TOOL_ROUNDS);
        assert_eq!(asked, MAX_TOOL_ROUNDS - 1, "the last round asks for nothing");
        let sent = frames(&mut out);
        assert!(
            sent.iter()
                .any(|frame| frame.contains("工具调用轮次过多，已停止")),
            "{sent:?}"
        );
        assert!(
            sent.iter().any(|frame| frame.contains("\"type\":\"done\"")),
            "the turn is closed rather than left disabled: {sent:?}"
        );
        assert!(call.assistant_message_id.is_none());
    }
}
