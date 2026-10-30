//! The push-to-talk session, as a state machine over frames.
//!
//! Ported from `web/scripts/voice.ts`, field by field, keeping the behaviour
//! whose reason that file already documents:
//!
//! * audio produced before the server answers `ready` is queued, never dropped —
//!   the first press has to open a session and the user has already started
//!   talking;
//! * a release while still connecting sets `pending_commit`, so the commit goes
//!   out after the queued audio, in order, instead of being swallowed;
//! * the 25 s cap is armed when audio actually starts flowing (at `ready`), not
//!   when the button went down, because the endpoint's buffer only fills then;
//! * an `error` frame blocks the rest of the hold. The endpoint already refused
//!   this utterance, and feeding it more of the same is what turned one refusal
//!   into hundreds of error lines on the page (docs/verification.md §7.1).
//!
//! The frame types come from `kestrel_proto` — the same types the server
//! serialises, which is the drift the hand-restated TS shapes allowed.
//!
//! No clock and no socket are reached for directly: callers pass `now_ms` and
//! implement [`Transport`], and the machine answers with [`Effect`] values. That
//! is what lets `cargo test` assert frame ordering without a browser, the job the
//! jsdom suite used to do.

use kestrel_proto::{ClientFrame, ServerFrame};

/// The endpoint's audio buffer caps at 30 s; a hold is committed at 25 s so the
/// tail of a long sentence is never rejected.
pub const MAX_HOLD_MS: u64 = 25_000;

/// Status strings. Every one of them is also an assertion in the tests below,
/// which is why they are named rather than inlined.
pub const STATUS_IDLE: &str = "按住按钮开始说话";
pub const STATUS_CONNECTING: &str = "正在连接…";
pub const STATUS_LISTENING: &str = "正在听…";
pub const STATUS_THINKING: &str = "正在思考…";
pub const STATUS_ANSWERING: &str = "正在回答…";
pub const STATUS_ENDED: &str = "会话已结束";

/// What the page has to do in the browser as a consequence of an event. The WASM
/// layer turns these into calls; the tests assert them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Fetch a single-use ticket and open the socket. Nothing is created until
    /// the first press: `getUserMedia` needs a user gesture, and opening an
    /// upstream session for a page that is merely being looked at would burn one.
    OpenSocket,
    StartCapture,
    SetStatus(&'static str),
    AppendEntry { kind: &'static str, text: String },
    /// `Some(id)` subscribes the conversation event stream — a long video render
    /// lands as a new message there, not on the socket; images do come inline on
    /// the socket. `None` closes it.
    ConversationStream(Option<i64>),
    SetButtonEnabled(bool),
    SetButtonActive(bool),
    PlayAudio { pcm: String },
    ShowAsset { kind: String, url: String },
}

/// The wire the machine drives: the WASM socket in the browser, a vector in tests.
pub trait Transport {
    fn send(&mut self, frame: &ClientFrame);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// No socket exists.
    Idle,
    /// A socket exists and the server answered `ready`.
    Ready,
    /// A socket exists but the session is not up yet.
    Connecting,
}

/// The whole page state, minus the DOM.
pub struct Session<S: Transport> {
    transport: S,
    phase: Phase,
    holding: bool,
    /// A turn is in flight (the endpoint is thinking or answering), so a new
    /// press is refused.
    busy: bool,
    /// This utterance is already committed, or was refused: stop feeding it.
    blocked: bool,
    queued: Vec<String>,
    pending_commit: bool,
    conversation_id: Option<i64>,
    /// When the current hold must be committed. `None` means nothing is armed.
    hold_deadline: Option<u64>,
}

impl<S: Transport> Session<S> {
    pub fn new(transport: S) -> Self {
        Session {
            transport,
            phase: Phase::Idle,
            holding: false,
            busy: false,
            blocked: false,
            queued: Vec::new(),
            pending_commit: false,
            conversation_id: None,
            hold_deadline: None,
        }
    }

    /// Borrow the transport: the WASM layer owns the socket through it, and the
    /// tests read what went out.
    pub fn transport(&self) -> &S {
        &self.transport
    }

    pub fn is_holding(&self) -> bool {
        self.holding
    }

    pub fn conversation_id(&self) -> Option<i64> {
        self.conversation_id
    }

    pub fn hold_deadline(&self) -> Option<u64> {
        self.hold_deadline
    }

    /// Pointer went down.
    pub fn press(&mut self, now_ms: u64) -> Vec<Effect> {
        if self.busy || self.holding {
            return Vec::new();
        }
        self.holding = true;
        self.blocked = false;
        let mut effects = vec![
            Effect::StartCapture,
            Effect::SetButtonActive(true),
            Effect::SetStatus(STATUS_LISTENING),
        ];
        match self.phase {
            Phase::Idle => {
                effects.insert(0, Effect::OpenSocket);
                // The status was written when the button went down, before there
                // was a session to listen to; say so until there is one.
                effects.push(Effect::SetStatus(STATUS_CONNECTING));
                self.hold_deadline = None;
            }
            Phase::Ready => self.arm(now_ms),
            Phase::Connecting => effects.push(Effect::SetStatus(STATUS_CONNECTING)),
        }
        effects
    }

    /// Pointer went up, or left the button.
    pub fn release(&mut self) -> Vec<Effect> {
        if !self.holding {
            return Vec::new();
        }
        self.holding = false;
        self.hold_deadline = None;
        let mut effects = vec![Effect::SetButtonActive(false)];
        if self.busy {
            // Already handed over by the cap, or a turn is running.
            return effects;
        }
        if self.blocked {
            // Committing again would submit an empty buffer on top of the
            // utterance the endpoint just refused.
            effects.push(Effect::SetStatus(STATUS_IDLE));
            return effects;
        }
        self.busy = true;
        match self.phase {
            Phase::Ready => {
                self.transport.send(&ClientFrame::Commit);
                effects.push(Effect::SetStatus(STATUS_THINKING));
            }
            Phase::Connecting | Phase::Idle => {
                // Still connecting: commit as soon as the session is up, so a
                // quick first press is not swallowed.
                self.pending_commit = true;
                effects.push(Effect::SetStatus(STATUS_CONNECTING));
            }
        }
        effects
    }

    /// One PCM16 chunk from the audio worklet, already base64-encoded.
    pub fn audio_chunk(&mut self, pcm: String) -> Vec<Effect> {
        if self.blocked {
            return Vec::new();
        }
        match self.phase {
            Phase::Ready => {
                self.transport.send(&ClientFrame::Audio { pcm });
                Vec::new()
            }
            Phase::Idle | Phase::Connecting => {
                self.queued.push(pcm);
                Vec::new()
            }
        }
    }

    /// The socket opened. This is where the call actually starts: the server
    /// creates no upstream session until `Start` arrives and says nothing when it
    /// does not, which is why a missing one looked like a healthy page.
    pub fn on_socket_open(&mut self) -> Vec<Effect> {
        if self.phase == Phase::Idle {
            self.phase = Phase::Connecting;
        }
        self.transport.send(&ClientFrame::Start);
        Vec::new()
    }

    pub fn on_socket_close(&mut self) -> Vec<Effect> {
        self.phase = Phase::Idle;
        self.queued.clear();
        self.pending_commit = false;
        self.holding = false;
        self.busy = false;
        self.blocked = false;
        self.hold_deadline = None;
        let mut effects = vec![
            Effect::ConversationStream(None),
            Effect::SetButtonEnabled(true),
            Effect::SetStatus(STATUS_IDLE),
        ];
        if self.conversation_id.is_some() {
            effects.push(Effect::AppendEntry {
                kind: "note",
                text: "会话已结束，再按一次继续".to_string(),
            });
            effects.push(Effect::SetStatus(STATUS_ENDED));
        }
        effects
    }

    /// A text frame off the socket. Anything that does not parse as a known
    /// server frame is ignored, as the script did: an unknown tag means the
    /// server moved on, and the page must not take the connection down over it.
    pub fn on_server_frame(&mut self, raw: &str, now_ms: u64) -> Vec<Effect> {
        let frame: ServerFrame = match serde_json::from_str(raw) {
            Ok(parsed) => parsed,
            Err(_) => return Vec::new(),
        };
        match frame {
            ServerFrame::Ready { conversation_id } => self.on_ready(conversation_id, now_ms),
            ServerFrame::Transcript { text } => vec![
                Effect::AppendEntry { kind: "user", text },
                Effect::SetStatus(STATUS_THINKING),
            ],
            ServerFrame::ToolCall { name, status } => {
                let text = if status == "running" {
                    format!("{name} 执行中…")
                } else {
                    format!("{name} 已完成")
                };
                vec![Effect::AppendEntry { kind: "tool", text }]
            }
            ServerFrame::Asset { kind, url, .. } => vec![Effect::ShowAsset {
                kind: kind.as_str().to_string(),
                url,
            }],
            ServerFrame::Audio { pcm } => vec![
                Effect::SetStatus(STATUS_ANSWERING),
                Effect::PlayAudio { pcm },
            ],
            ServerFrame::AssistantText { text } => {
                vec![Effect::AppendEntry { kind: "assistant", text }]
            }
            ServerFrame::Done => {
                self.busy = false;
                vec![Effect::SetButtonEnabled(true), Effect::SetStatus(STATUS_IDLE)]
            }
            ServerFrame::Error { message } => {
                // Whatever the endpoint refused, more of the same will not help.
                self.blocked = true;
                self.busy = false;
                self.hold_deadline = None;
                vec![
                    Effect::AppendEntry { kind: "error", text: message },
                    Effect::SetButtonEnabled(true),
                ]
            }
        }
    }

    /// Drive the hold cap. The WASM layer calls this from a timer; tests call it
    /// with explicit timestamps.
    pub fn tick(&mut self, now_ms: u64) -> Vec<Effect> {
        let due = self
            .hold_deadline
            .map(|deadline| now_ms >= deadline)
            .unwrap_or(false);
        if !due || !self.holding || self.blocked {
            return Vec::new();
        }
        // Hand over what has been said and stop feeding. The user is told,
        // because the alternative is the audio after this point going nowhere.
        self.blocked = true;
        self.busy = true;
        self.hold_deadline = None;
        self.transport.send(&ClientFrame::Commit);
        vec![
            Effect::AppendEntry {
                kind: "note",
                text: "已到单段上限，这段先提交了；还有话要说请松开再按一次".to_string(),
            },
            Effect::SetStatus(STATUS_THINKING),
        ]
    }

    /// The page is going away: stop the session without waiting for the server.
    pub fn shutdown(&mut self) -> Vec<Effect> {
        self.transport.send(&ClientFrame::Stop);
        vec![Effect::ConversationStream(None)]
    }

    fn on_ready(&mut self, conversation_id: i64, now_ms: u64) -> Vec<Effect> {
        self.phase = Phase::Ready;
        self.conversation_id = Some(conversation_id);
        // A long render lands on the conversation stream, not on this socket.
        let mut effects = vec![Effect::ConversationStream(Some(conversation_id))];
        for pcm in std::mem::take(&mut self.queued) {
            self.transport.send(&ClientFrame::Audio { pcm });
        }
        if self.pending_commit {
            self.pending_commit = false;
            self.transport.send(&ClientFrame::Commit);
        }
        if self.holding {
            effects.push(Effect::SetStatus(STATUS_LISTENING));
            self.arm(now_ms);
        }
        effects
    }

    fn arm(&mut self, now_ms: u64) {
        self.hold_deadline = Some(now_ms.saturating_add(MAX_HOLD_MS));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records what the machine put on the wire, in order.
    #[derive(Default)]
    struct Recorder {
        frames: Vec<ClientFrame>,
    }

    impl Transport for Recorder {
        fn send(&mut self, frame: &ClientFrame) {
            self.frames.push(frame.clone());
        }
    }

    fn session() -> Session<Recorder> {
        Session::new(Recorder::default())
    }

    fn frames(session: &Session<Recorder>) -> Vec<ClientFrame> {
        session.transport().frames.clone()
    }

    const READY: &str = r#"{"type":"ready","conversationId":7}"#;

    /// The assertion that exists because of an outage: opening the socket is not
    /// the same as starting a call. `start` must be the first frame out, or the
    /// server opens nothing and the page waits forever on "正在连接…" with a
    /// clean console and a healthy socket.
    #[test]
    fn the_first_frame_on_the_wire_is_start() {
        let mut session = session();
        session.press(0);
        session.on_socket_open();
        assert_eq!(frames(&session), vec![ClientFrame::Start]);
    }

    #[test]
    fn press_opens_the_socket_before_it_starts_capturing() {
        let mut session = session();
        let effects = session.press(0);
        assert_eq!(effects.first(), Some(&Effect::OpenSocket));
        assert!(effects.contains(&Effect::StartCapture));
        assert_eq!(
            effects.last(),
            Some(&Effect::SetStatus(STATUS_CONNECTING)),
            "there is no session to listen to yet, so the page must not claim it is listening"
        );
    }

    /// Audio produced while the session is coming up belongs to the utterance;
    /// dropping it is what made a quick first press say nothing at all.
    #[test]
    fn audio_before_ready_is_queued_then_flushed_in_order() {
        let mut session = session();
        session.press(0);
        session.on_socket_open();
        session.audio_chunk("AAA=".to_string());
        session.audio_chunk("BBB=".to_string());
        assert_eq!(frames(&session), vec![ClientFrame::Start]);

        session.on_server_frame(READY, 100);
        assert_eq!(
            frames(&session),
            vec![
                ClientFrame::Start,
                ClientFrame::Audio { pcm: "AAA=".to_string() },
                ClientFrame::Audio { pcm: "BBB=".to_string() },
            ]
        );
    }

    /// A release that lands before `ready` defers the commit, and the commit must
    /// come after the queued audio, never before it.
    #[test]
    fn release_before_ready_defers_the_commit_behind_the_flush() {
        let mut session = session();
        session.press(0);
        session.on_socket_open();
        session.audio_chunk("AAA=".to_string());
        let effects = session.release();
        assert_eq!(
            effects.last(),
            Some(&Effect::SetStatus(STATUS_CONNECTING)),
            "the hold is over but the utterance is not handed over yet"
        );
        assert_eq!(frames(&session), vec![ClientFrame::Start]);

        session.on_server_frame(READY, 100);
        assert_eq!(
            frames(&session),
            vec![
                ClientFrame::Start,
                ClientFrame::Audio { pcm: "AAA=".to_string() },
                ClientFrame::Commit,
            ]
        );
    }

    #[test]
    fn release_after_ready_commits_at_once() {
        let mut session = session();
        session.press(0);
        session.on_socket_open();
        session.on_server_frame(READY, 0);
        session.audio_chunk("AAA=".to_string());
        session.release();
        assert_eq!(
            frames(&session),
            vec![
                ClientFrame::Start,
                ClientFrame::Audio { pcm: "AAA=".to_string() },
                ClientFrame::Commit,
            ]
        );
    }

    /// The cap is armed at `ready`, not at press: the endpoint's buffer only
    /// starts filling when audio starts flowing.
    #[test]
    fn the_cap_is_armed_when_audio_starts_flowing() {
        let mut session = session();
        session.press(0);
        assert_eq!(session.hold_deadline(), None);
        session.on_socket_open();
        session.on_server_frame(READY, 1_000);
        assert_eq!(session.hold_deadline(), Some(1_000 + MAX_HOLD_MS));
    }

    #[test]
    fn a_long_hold_is_committed_at_the_cap_and_then_stops_feeding() {
        let mut session = session();
        session.press(0);
        session.on_socket_open();
        session.on_server_frame(READY, 0);

        let effects = session.tick(MAX_HOLD_MS);
        assert_eq!(effects.last(), Some(&Effect::SetStatus(STATUS_THINKING)));
        assert_eq!(
            frames(&session),
            vec![ClientFrame::Start, ClientFrame::Commit]
        );

        session.audio_chunk("CCC=".to_string());
        assert_eq!(
            frames(&session),
            vec![ClientFrame::Start, ClientFrame::Commit],
            "past the cap the endpoint holds 30 s of buffer; more would be refused"
        );

        // Releasing must not commit a second time on top of the first handover.
        session.release();
        assert_eq!(
            frames(&session),
            vec![ClientFrame::Start, ClientFrame::Commit]
        );
    }

    /// One refusal used to print hundreds of error lines, because every frame
    /// after it was refused again. Feeding stops at the first one.
    #[test]
    fn an_error_frame_blocks_the_rest_of_the_hold() {
        let mut session = session();
        session.press(0);
        session.on_socket_open();
        session.on_server_frame(READY, 0);
        session.on_server_frame(r#"{"type":"error","message":"buffer exceeded"}"#, 0);

        session.audio_chunk("AAA=".to_string());
        session.audio_chunk("BBB=".to_string());
        assert_eq!(
            frames(&session),
            vec![ClientFrame::Start],
            "no audio after the refusal, and no second commit"
        );
        let effects = session.release();
        assert_eq!(
            effects.last(),
            Some(&Effect::SetStatus(STATUS_IDLE)),
            "the page goes back to waiting for a new press"
        );
    }

    #[test]
    fn frames_that_do_not_parse_or_are_unknown_are_ignored() {
        let mut session = session();
        for raw in ["", "not json", "[]", "null", r#"{"type":"nope"}"#, r#"{"pcm":"AAA="}"#] {
            assert!(
                session.on_server_frame(raw, 0).is_empty(),
                "{raw} must be ignored, not panic"
            );
        }
        assert!(frames(&session).is_empty());
    }

    #[test]
    fn a_press_is_refused_while_a_turn_is_running() {
        let mut session = session();
        session.press(0);
        session.on_socket_open();
        session.on_server_frame(READY, 0);
        session.release();
        assert!(
            session.press(MAX_HOLD_MS).is_empty(),
            "busy: the endpoint is still answering"
        );

        let effects = session.on_server_frame(r#"{"type":"done"}"#, MAX_HOLD_MS);
        assert!(effects.contains(&Effect::SetButtonEnabled(true)));
        assert!(
            !session.press(MAX_HOLD_MS + 1).is_empty(),
            "done frees the button"
        );
    }

    #[test]
    fn ready_subscribes_the_conversation_stream_and_close_unsubscribes() {
        let mut session = session();
        session.press(0);
        session.on_socket_open();
        let effects = session.on_server_frame(READY, 0);
        assert_eq!(
            effects.first(),
            Some(&Effect::ConversationStream(Some(7))),
            "a long video render lands on that stream, not on this socket"
        );
        assert_eq!(session.conversation_id(), Some(7));

        let effects = session.on_socket_close();
        assert!(effects.contains(&Effect::ConversationStream(None)));
        assert!(effects.contains(&Effect::AppendEntry {
            kind: "note",
            text: "会话已结束，再按一次继续".to_string()
        }));
    }

    #[test]
    fn leaving_the_page_stops_the_session() {
        let mut session = session();
        session.press(0);
        session.on_socket_open();
        session.shutdown();
        assert_eq!(frames(&session).last(), Some(&ClientFrame::Stop));
    }

    /// The asset frame carries the kind the page keys its preview on.
    #[test]
    fn an_asset_frame_becomes_a_preview() {
        let mut session = session();
        let effects = session.on_server_frame(
            r#"{"type":"asset","id":3,"kind":"video","url":"/api/v1/assets/3/download","mime":"video/mp4","bytes":8}"#,
            0,
        );
        assert_eq!(
            effects,
            vec![Effect::ShowAsset {
                kind: "video".to_string(),
                url: "/api/v1/assets/3/download".to_string()
            }]
        );
    }
}
