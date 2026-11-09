//! The push-to-talk session, as a state machine over frames.
//!
//! Ported from `nest/client/scripts/voice.ts`, field by field, keeping the behaviour
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
//! * pressing the button **while the assistant is still speaking** interrupts it:
//!   the speaker goes silent at once, the turn is cancelled on the socket, and the
//!   new hold's utterance is handed over only once the interrupted turn has
//!   actually closed (docs/architecture.md §11.6).
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

/// How often the page must call [`Session::tick`]. The cap is stored as an
/// absolute deadline, so the interval only has to be short enough to notice it —
/// 250 ms is well inside the 5 s of slack between 25 s and the endpoint's 30 s.
pub const TICK_MS: i32 = 250;

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
    /// Silence the speaker now: everything queued for the interrupted answer is
    /// dropped. Paired with [`Effect::PlayAudio`] being withheld while the page is
    /// muted, which is what keeps a cancelled answer from restarting itself.
    StopPlayback,
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
    /// How many PCM chunks this hold has produced. The guard against committing an
    /// empty buffer reads this, and `press` resets it — the count is per hold, not
    /// per session, because the failure is per press.
    frames_this_hold: u32,
    /// The answer being spoken right now: audio arrived for this turn and the turn
    /// has not closed. A press is only an interruption when this is true — that is
    /// the one state where the user hears something they may want to stop, and the
    /// one state in which the server has a response to cancel.
    spoke: bool,
    /// The page must not play anything: set when the user interrupts, cleared when
    /// the interrupted turn closes. Without it the chunks already on their way from
    /// the cancelled answer would resume the very speech they just stopped.
    muted: bool,
    /// This hold **interrupted** a turn that is still owned by the server, so its
    /// commit has to wait for that turn to close. Committing earlier is not merely
    /// early — the server refuses an utterance while a turn is running, and the
    /// words would vanish without a trace.
    interrupting: bool,
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
            frames_this_hold: 0,
            spoke: false,
            muted: false,
            interrupting: false,
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
        if self.holding {
            return Vec::new();
        }
        if self.busy {
            // A press during a turn is only refused while there is nothing to hear.
            // Once the answer is speaking the user is talking over it, and that is
            // an interruption, not a queue jump (docs/architecture.md §11.6).
            return if self.spoke { self.interrupt(now_ms) } else { Vec::new() };
        }
        self.holding = true;
        self.blocked = false;
        self.frames_this_hold = 0;
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

    /// Pointer went down **while the assistant was speaking**: go silent, tell the
    /// server to stop talking, and start collecting the new utterance.
    ///
    /// `busy` deliberately stays set. The turn is not ours to hand over until the
    /// server says it closed it — see [`Session::close_turn`] — and an empty
    /// interrupt is still an interrupt, so `release` reads `frames_this_hold` the
    /// same way it always has.
    fn interrupt(&mut self, now_ms: u64) -> Vec<Effect> {
        self.holding = true;
        self.blocked = false;
        self.frames_this_hold = 0;
        self.muted = true;
        self.interrupting = true;
        self.transport.send(&ClientFrame::Cancel);
        let mut effects = vec![
            Effect::StopPlayback,
            Effect::SetButtonActive(true),
            Effect::SetStatus(STATUS_LISTENING),
        ];
        match self.phase {
            Phase::Ready => self.arm(now_ms),
            // Defensive: `busy` with no session cannot be reached through the
            // public methods, and if it ever is, the hold must not claim to be
            // listening to an endpoint that is not there.
            Phase::Idle | Phase::Connecting => effects.push(Effect::SetStatus(STATUS_CONNECTING)),
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
        if self.interrupting {
            if self.frames_this_hold == 0 {
                self.interrupting = false;
                return self.end_hold_without_audio(effects);
            }
            // The interrupted turn has not closed yet, and the server refuses an
            // utterance while a turn is running — committing now would lose the
            // words without a trace. `close_turn` sends it the moment they can land.
            self.interrupting = false;
            self.pending_commit = true;
            effects.push(Effect::SetStatus(STATUS_THINKING));
            return effects;
        }
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
        if self.frames_this_hold == 0 {
            return self.end_hold_without_audio(effects);
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
        // Counted for every chunk the machine accepts, whether it goes out at once
        // or lands in `queued` — the guard asks "did this press produce audio", not
        // "did it reach the socket yet".
        self.frames_this_hold += 1;
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
        self.spoke = false;
        self.muted = false;
        self.interrupting = false;
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
            ServerFrame::Audio { pcm } => {
                if self.muted {
                    // A cancelled answer does not stop the instant the endpoint is
                    // told to: the chunks already on the wire would restart the very
                    // speech the user just interrupted.
                    return Vec::new();
                }
                self.spoke = true;
                vec![Effect::SetStatus(STATUS_ANSWERING), Effect::PlayAudio { pcm }]
            }
            ServerFrame::AssistantText { text } => {
                vec![Effect::AppendEntry { kind: "assistant", text }]
            }
            ServerFrame::Done => self.close_turn(),
            ServerFrame::Error { message } => {
                // Whatever the endpoint refused, more of the same will not help.
                self.blocked = true;
                self.busy = false;
                self.hold_deadline = None;
                self.spoke = false;
                self.muted = false;
                self.interrupting = false;
                // An utterance held back behind an interrupted turn has nowhere to
                // go now: the refusal is about this call, and re-sending into it is
                // how one refusal became a wall of them (docs/verification.md §7.1).
                self.pending_commit = false;
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
        if self.frames_this_hold == 0 {
            // A full 25 s of holding with not one frame out of the microphone is the
            // same empty-buffer commit the release path refuses. The cap must not be
            // the hole through which it comes back.
            self.holding = false;
            return self.end_hold_without_audio(vec![Effect::SetButtonActive(false)]);
        }
        // Hand over what has been said and stop feeding. The user is told,
        // because the alternative is the audio after this point going nowhere.
        self.blocked = true;
        self.busy = true;
        self.hold_deadline = None;
        if self.interrupting {
            // The cap ends the hold, not the interruption: the turn being
            // interrupted still owns the server, so this commit waits for it too.
            self.interrupting = false;
            self.pending_commit = true;
        } else {
            self.transport.send(&ClientFrame::Commit);
        }
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

    /// End the hold **without** handing anything over: the microphone could not be
    /// opened, so there is no utterance to commit.
    ///
    /// The Nest client did not have this path — its `press()` error handler called
    /// `release()`, which queued a commit of an empty buffer, and the endpoint
    /// answered "没有听清，请再说一次" for a failure that was the page's, not the
    /// user's. A permission denial must not spend a turn.
    pub fn abort(&mut self, message: &str) -> Vec<Effect> {
        self.holding = false;
        self.blocked = true;
        self.busy = false;
        self.pending_commit = false;
        self.spoke = false;
        self.muted = false;
        self.interrupting = false;
        self.queued.clear();
        self.hold_deadline = None;
        vec![
            Effect::AppendEntry { kind: "error", text: message.to_string() },
            Effect::SetButtonActive(false),
            Effect::SetButtonEnabled(true),
            Effect::SetStatus(STATUS_IDLE),
        ]
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

    /// The turn that was running has closed.
    ///
    /// Three things happen here that cannot happen anywhere else: the button comes
    /// back, the page stops being muted, and an utterance that was held back — by an
    /// interruption, or by a session that was still coming up — is finally handed
    /// over. The server refuses a commit while a turn runs, so this is the earliest
    /// moment at which the words are not lost.
    fn close_turn(&mut self) -> Vec<Effect> {
        self.busy = false;
        self.spoke = false;
        self.muted = false;
        self.interrupting = false;
        let mut effects = vec![Effect::SetButtonEnabled(true)];
        if self.pending_commit {
            self.pending_commit = false;
            self.busy = true;
            self.transport.send(&ClientFrame::Commit);
            effects.push(Effect::SetStatus(STATUS_THINKING));
            return effects;
        }
        // A hold that is still feeding keeps "正在听…": the answer it was waiting
        // for was interrupted, and the microphone is still open.
        let listening = self.holding && !self.blocked;
        effects.push(Effect::SetStatus(if listening {
            STATUS_LISTENING
        } else {
            STATUS_IDLE
        }));
        effects
    }

    /// Ends a hold that produced **no audio at all**, without handing anything over.
    ///
    /// Not the `abort` path: there, the page knows capture failed — it asked for the
    /// microphone and was refused. Here capture was asked for, nothing reported an
    /// error, and no frame ever arrived. That is what a hold landing on the browser's
    /// permission prompt looks like from inside the page (`getUserMedia` yields nothing
    /// until the prompt is answered). Measured on the first real voice dial on the new
    /// chain: two presses, `frames=0` both times, and the endpoint answered
    /// `buffer too small, or have no audio` (docs/verification.md §25.1).
    ///
    /// Committing an empty buffer spends a turn — one upstream session, one refusal
    /// printed on the page — for a failure that is the page's, not the speaker's. So
    /// the hold is closed instead: `blocked` stops late frames from this press leaking
    /// into the *next* commit, `queued` is dropped for the same reason, and the button
    /// is handed back. `press` clears `blocked`, so the next press is unaffected.
    fn end_hold_without_audio(&mut self, mut effects: Vec<Effect>) -> Vec<Effect> {
        self.blocked = true;
        self.pending_commit = false;
        self.interrupting = false;
        self.queued.clear();
        self.hold_deadline = None;
        effects.push(Effect::AppendEntry {
            kind: "note",
            text: "这一按没有采到声音，没有提交；再按住说一次就行".to_string(),
        });
        effects.push(Effect::SetButtonEnabled(true));
        effects.push(Effect::SetStatus(STATUS_IDLE));
        effects
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
        // There has to be an utterance to hand over. A 25 s hold that produced no
        // audio at all takes the empty-hold path instead (see
        // `a_hold_that_never_captured_is_not_committed_at_the_cap`).
        session.audio_chunk("AAA=".to_string());

        let effects = session.tick(MAX_HOLD_MS);
        assert_eq!(effects.last(), Some(&Effect::SetStatus(STATUS_THINKING)));
        assert_eq!(
            frames(&session),
            vec![
                ClientFrame::Start,
                ClientFrame::Audio { pcm: "AAA=".to_string() },
                ClientFrame::Commit,
            ]
        );

        session.audio_chunk("CCC=".to_string());
        assert_eq!(
            frames(&session),
            vec![
                ClientFrame::Start,
                ClientFrame::Audio { pcm: "AAA=".to_string() },
                ClientFrame::Commit
            ],
            "past the cap the endpoint holds 30 s of buffer; more would be refused"
        );

        // Releasing must not commit a second time on top of the first handover.
        session.release();
        assert_eq!(
            frames(&session),
            vec![
                ClientFrame::Start,
                ClientFrame::Audio { pcm: "AAA=".to_string() },
                ClientFrame::Commit
            ]
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
        // A turn only runs if something was handed over; an empty hold is refused
        // instead (see `a_hold_that_produced_no_audio_is_never_committed`).
        session.audio_chunk("AAA=".to_string());
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

    /// A permission denial is the page's failure, not an empty utterance: the
    /// Nest client committed anyway and the endpoint answered "没有听清，请再说一次".
    #[test]
    fn a_hold_that_produced_no_audio_is_never_committed() {
        // The first real voice dial on the new chain: two presses, `frames=0` both
        // times, each answered upstream with "buffer too small, or have no audio"
        // (docs/verification.md §25.1). A press landing on the browser's permission
        // prompt produces nothing, and nothing inside the page distinguishes that from
        // a capture that is working — so the machine refuses the commit.
        let mut session = session();
        session.press(0);
        session.on_socket_open();
        session.on_server_frame(READY, 0);

        let effects = session.release();
        assert_eq!(
            frames(&session),
            vec![ClientFrame::Start],
            "no Commit: there is no utterance to hand over"
        );
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect, Effect::AppendEntry { .. })),
            "the page has to say so — a silent no-op reads as a dead button"
        );
        assert!(effects.contains(&Effect::SetButtonEnabled(true)));
        assert!(effects.contains(&Effect::SetStatus(STATUS_IDLE)));
        assert!(!session.is_holding(), "the hold is closed");

        // The next press works: `press` clears the block this path sets, so one
        // refusal does not cost the rest of the call.
        session.press(5_000);
        session.audio_chunk("AAA=".to_string());
        session.release();
        assert_eq!(
            frames(&session),
            vec![
                ClientFrame::Start,
                ClientFrame::Audio { pcm: "AAA=".to_string() },
                ClientFrame::Commit,
            ],
            "a hold that did capture audio still commits"
        );
    }

    #[test]
    fn a_hold_that_never_captured_is_not_committed_at_the_cap() {
        // The 25 s cap hands over a long sentence. It must not become the hole through
        // which the empty commit comes back: no frames, no utterance, no Commit.
        let mut session = session();
        session.press(0);
        session.on_socket_open();
        session.on_server_frame(READY, 0);

        let effects = session.tick(MAX_HOLD_MS);
        assert_eq!(
            frames(&session),
            vec![ClientFrame::Start],
            "the cap does not commit an empty hold"
        );
        assert!(
            !effects.contains(&Effect::SetStatus(STATUS_THINKING)),
            "nothing is in flight, so the status must not claim otherwise"
        );
        assert!(effects.contains(&Effect::SetStatus(STATUS_IDLE)));
        assert!(!session.is_holding());
        assert_eq!(
            session.hold_deadline(),
            None,
            "a closed hold leaves no armed deadline behind"
        );
    }

    #[test]
    fn frames_arriving_after_an_empty_hold_do_not_reach_the_next_commit() {
        // The worklet can hand a chunk over just after the button came up. Left in
        // `queued` it would be flushed into the user's *next* sentence, so the block
        // this path sets drops it instead.
        let mut session = session();
        session.press(0);
        session.on_socket_open();
        session.release();
        assert_eq!(frames(&session), vec![ClientFrame::Start]);

        session.audio_chunk("LATE=".to_string());
        assert_eq!(
            frames(&session),
            vec![ClientFrame::Start],
            "a late frame from a refused hold is dropped, not queued"
        );
    }

    #[test]
    fn an_uncapturable_press_is_abandoned_without_committing() {
        let mut session = session();
        session.press(0);
        session.on_socket_open();
        session.on_server_frame(READY, 0);

        let effects = session.abort("拿不到麦克风权限，无法说话");
        assert_eq!(
            frames(&session),
            vec![ClientFrame::Start],
            "no commit: there is nothing to hand over"
        );
        assert!(effects.contains(&Effect::SetStatus(STATUS_IDLE)));
        assert!(effects.contains(&Effect::SetButtonEnabled(true)));

        // The next press must work, which is the point of not leaving a commit
        // queued behind a failure.
        session.press(1_000);
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

    const AUDIO: &str = r#"{"type":"audio","pcm":"AAA="}"#;
    const DONE: &str = r#"{"type":"done"}"#;

    /// Brings a session up to the point where the assistant is speaking: press,
    /// connect, ready, one utterance committed, and the first chunk of the answer.
    fn speaking_session() -> Session<Recorder> {
        let mut session = session();
        session.press(0);
        session.on_socket_open();
        session.on_server_frame(READY, 0);
        session.audio_chunk("AAA=".to_string());
        session.release();
        session.on_server_frame(AUDIO, 0);
        session
    }

    /// The barge-in. Before this the press was swallowed and the answer went on
    /// talking over the user (docs/architecture.md §11.6).
    #[test]
    fn pressing_while_the_answer_speaks_interrupts_it() {
        let mut session = speaking_session();
        let effects = session.press(1_000);
        assert_eq!(effects.first(), Some(&Effect::StopPlayback));
        assert!(effects.contains(&Effect::SetButtonActive(true)));
        assert_eq!(
            effects.last(),
            Some(&Effect::SetStatus(STATUS_LISTENING)),
            "the page is now listening to the user, not playing"
        );
        assert!(session.is_holding(), "the interrupting press is a hold like any other");
        assert_eq!(
            session.hold_deadline(),
            Some(1_000 + MAX_HOLD_MS),
            "and it is capped like any other"
        );
        assert_eq!(
            session.transport().frames.last(),
            Some(&ClientFrame::Cancel),
            "the server is told to stop talking, after the commit of the last utterance"
        );
    }

    /// A press is only an interruption when there is something to hear. While the
    /// model is thinking — or running a tool — there is no speech to stop, and the
    /// server would refuse the commit that follows anyway, so the press stays a
    /// no-op exactly as it was before.
    #[test]
    fn pressing_while_the_model_is_thinking_is_still_refused() {
        let mut session = session();
        session.press(0);
        session.on_socket_open();
        session.on_server_frame(READY, 0);
        session.audio_chunk("AAA=".to_string());
        session.release();
        session.on_server_frame(r#"{"type":"transcript","text":"画一只红隼"}"#, 0);
        assert!(session.press(1_000).is_empty(), "nothing is playing");
        assert!(!session.is_holding());
        assert_eq!(
            session.transport().frames.last(),
            Some(&ClientFrame::Commit),
            "and no cancel went out either"
        );
    }

    /// A cancelled answer does not stop the instant the endpoint is told to: the
    /// chunks already on the wire would restart the speech the user interrupted.
    #[test]
    fn the_rest_of_a_cancelled_answer_is_not_played() {
        let mut session = speaking_session();
        session.press(1_000);
        assert!(
            session.on_server_frame(AUDIO, 1_500).is_empty(),
            "muted until the interrupted turn closes"
        );
        session.on_server_frame(DONE, 1_600);
        assert_eq!(
            session.on_server_frame(AUDIO, 2_000),
            vec![Effect::SetStatus(STATUS_ANSWERING), Effect::PlayAudio { pcm: "AAA=".to_string() }],
            "the next answer plays"
        );
    }

    /// The words spoken over an answer are handed over when the interrupted turn
    /// closes — not before. Committing early is not merely early: the server refuses
    /// an utterance while a turn runs, and the user would hear nothing at all.
    #[test]
    fn an_interrupting_hold_hands_over_its_words_when_the_turn_closes() {
        let mut session = speaking_session();
        session.press(1_000);
        session.audio_chunk("BBB=".to_string());
        let effects = session.release();
        assert_eq!(effects.last(), Some(&Effect::SetStatus(STATUS_THINKING)));
        assert_eq!(
            session.transport().frames,
            vec![
                ClientFrame::Start,
                ClientFrame::Audio { pcm: "AAA=".to_string() },
                ClientFrame::Commit,
                ClientFrame::Cancel,
                ClientFrame::Audio { pcm: "BBB=".to_string() },
            ],
            "the audio went out as it was captured; the commit is what is held"
        );

        session.on_server_frame(DONE, 2_000);
        assert_eq!(
            session.transport().frames,
            vec![
                ClientFrame::Start,
                ClientFrame::Audio { pcm: "AAA=".to_string() },
                ClientFrame::Commit,
                ClientFrame::Cancel,
                ClientFrame::Audio { pcm: "BBB=".to_string() },
                ClientFrame::Commit,
            ],
            "the held audio went out before the deferred commit"
        );
    }

    /// The 25 s cap hands over an interrupting hold the same way a release does:
    /// the audio goes, the commit waits for the turn being interrupted to close.
    #[test]
    fn the_cap_defers_an_interrupting_hold_too() {
        let mut session = speaking_session();
        session.press(1_000);
        session.audio_chunk("CCC=".to_string());
        // The cap fires with an utterance behind it: deferred like `release`.
        let effects = session.tick(1_000 + MAX_HOLD_MS);
        assert_eq!(effects.last(), Some(&Effect::SetStatus(STATUS_THINKING)));
        assert_eq!(
            session.transport().frames.last(),
            Some(&ClientFrame::Audio { pcm: "CCC=".to_string() }),
            "no commit ahead of the turn still running"
        );
        session.on_server_frame(DONE, 2_000);
        assert_eq!(session.transport().frames.last(), Some(&ClientFrame::Commit));
    }

    /// A hold that is still feeding keeps "正在听…" when the turn it was waiting on
    /// closes; it is the answer that ended, not the press.
    #[test]
    fn a_close_while_still_holding_keeps_the_page_listening() {
        let mut session = session();
        session.press(0);
        session.on_socket_open();
        session.on_server_frame(READY, 0);
        session.audio_chunk("AAA=".to_string());
        session.release();
        assert_eq!(
            session.on_server_frame(DONE, 1_000),
            vec![Effect::SetButtonEnabled(true), Effect::SetStatus(STATUS_IDLE)],
            "released: the hold is over"
        );

        let mut holding = speaking_session();
        holding.press(1_000);
        assert_eq!(
            holding.on_server_frame(DONE, 1_500),
            vec![Effect::SetButtonEnabled(true), Effect::SetStatus(STATUS_LISTENING)],
            "the user is still talking over the answer"
        );
    }
}
