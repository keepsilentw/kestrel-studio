//! The conversation page's decisions, without a DOM.
//!
//! Same split as [`crate::voice_core`]: the browser half is mechanical, this half is
//! where the bugs are. Two rules here have produced real defects:
//!
//! * **an event must not paint twice.** The SSR shell already renders the stored
//!   turns and stamps each with its message id; the stream then replays from the
//!   cursor the shell handed over (`data-start-seq`). Without the rendered set a
//!   reload shows every turn twice; with a stale id set the live turn never shows.
//! * **a cursor the journal no longer has is not a gap to paper over.** The server
//!   answers `resync`, and the only correct reaction is to reload the page, because
//!   the missing events cannot be reconstructed here (architecture.md §4.2).
//!
//! The download maths is here too, because it is arithmetic the page gets visibly
//! wrong: the longest edge scales to the requested size, both edges stay at least
//! 1 px, and PNG ignores the quality slider.
//!
//! The event type is `kestrel_proto`'s, not a restatement: the server used to be
//! able to rename a field and the page would just stop painting.

use std::collections::HashSet;

use kestrel_proto::event::{ConversationEvent, MessageView, Mode};

/// Validate the mode read back from `localStorage`. An unrecognised value yields
/// `None` and the page keeps the shell's own choice, so the selector can never
/// disagree with what the server rendered.
pub fn stored_mode(raw: Option<&str>) -> Option<Mode> {
    Mode::from_storage(raw)
}

/// What the DOM layer is asked to do, in the order the events arrived. The stream
/// is the only source of order; the page never re-sorts it.
#[derive(Clone, Debug, PartialEq)]
pub enum Paint {
    /// A message the shell did not render: a live user echo, or one the background
    /// worker appended when a video finished.
    RenderMessage { message: MessageView },
    /// Open an assistant placeholder for a turn that is starting.
    OpenTurn { message_id: i64, mode: Option<Mode> },
    Reasoning { message_id: i64, delta: String },
    Text { message_id: i64, delta: String },
    ToolCall { message_id: i64, name: String, status: String },
    ToolResult { message_id: i64, name: String, ok: bool, summary: String },
    Asset { message_id: i64, kind: String, url: String },
    TurnDone { message_id: i64 },
    TurnError { message_id: i64, message: String },
    TaskUpdated { task_id: i64, kind: String, status: String, error: Option<String> },
    /// The cursor is behind the journal's retained window: reload, do not guess.
    Reload,
}

pub struct Stream {
    /// Messages the shell already painted, plus messages this stream painted.
    rendered: HashSet<i64>,
}

impl Stream {
    pub fn new() -> Self {
        Self { rendered: HashSet::new() }
    }

    /// Seed with the ids the SSR shell put on the page, read from the
    /// `data-message-id` of each rendered turn at startup.
    pub fn note_rendered(&mut self, message_id: i64) {
        self.rendered.insert(message_id);
    }

    pub fn is_rendered(&self, message_id: i64) -> bool {
        self.rendered.contains(&message_id)
    }

    /// Fold one journaled event into paint instructions. Whole-message events are
    /// idempotent per message; deltas pass through, because replaying a delta the
    /// page has not seen is the point of the journal.
    pub fn on_event(&mut self, event: &ConversationEvent) -> Vec<Paint> {
        match event {
            ConversationEvent::UserMessage { message }
            | ConversationEvent::MessageAdded { message } => {
                if self.rendered.contains(&message.id) {
                    return Vec::new();
                }
                self.rendered.insert(message.id);
                vec![Paint::RenderMessage { message: message.clone() }]
            }
            ConversationEvent::TurnStarted { message_id, mode } => {
                if self.rendered.contains(message_id) {
                    return Vec::new();
                }
                self.rendered.insert(*message_id);
                vec![Paint::OpenTurn { message_id: *message_id, mode: *mode }]
            }
            ConversationEvent::Reasoning { message_id, delta } => {
                vec![Paint::Reasoning { message_id: *message_id, delta: delta.clone() }]
            }
            ConversationEvent::Text { message_id, delta } => {
                vec![Paint::Text { message_id: *message_id, delta: delta.clone() }]
            }
            ConversationEvent::ToolCall { message_id, name, status, .. } => {
                vec![Paint::ToolCall {
                    message_id: *message_id,
                    name: name.clone(),
                    status: status.clone(),
                }]
            }
            ConversationEvent::ToolResult { message_id, name, ok, summary } => {
                vec![Paint::ToolResult {
                    message_id: *message_id,
                    name: name.clone(),
                    ok: *ok,
                    summary: summary.clone(),
                }]
            }
            ConversationEvent::Asset { message_id, kind, url, .. } => {
                vec![Paint::Asset {
                    message_id: *message_id,
                    kind: kind.as_str().to_string(),
                    url: url.clone(),
                }]
            }
            ConversationEvent::TurnDone { message_id } => {
                vec![Paint::TurnDone { message_id: *message_id }]
            }
            ConversationEvent::TurnError { message_id, message } => {
                vec![Paint::TurnError { message_id: *message_id, message: message.clone() }]
            }
            ConversationEvent::TaskUpdated { task_id, kind, status, error } => {
                vec![Paint::TaskUpdated {
                    task_id: *task_id,
                    kind: kind.as_str().to_string(),
                    status: status.as_str().to_string(),
                    error: error.clone(),
                }]
            }
        }
    }

    /// The `resync` frame is not a `ConversationEvent`: it is the server's answer to
    /// a cursor it can no longer replay from, and it arrives before `connected`.
    pub const RESYNC_EVENT: &'static str = "resync";

    pub fn paint_for_name(name: &str) -> Option<Paint> {
        (name == Self::RESYNC_EVENT).then_some(Paint::Reload)
    }
}

impl Default for Stream {
    fn default() -> Self {
        Self::new()
    }
}

/// A re-encode request the download dialog can act on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DownloadPlan {
    pub width: u32,
    pub height: u32,
    /// The quality argument for `canvas.toBlob`. PNG ignores it, so this is `1.0`
    /// there rather than whatever the slider says.
    pub quality: f32,
    /// Whether the dialog should offer a size estimate at all: a lossless PNG
    /// re-encode does not shrink with a slider the format never reads.
    pub estimates_size: bool,
}

/// Scale one edge, keeping the promise that no edge rounds to zero — a 0-height
/// canvas exports as a blank image without saying so.
fn scale_edge(value: u32, size: u32, longest: u32) -> u32 {
    let scaled = (f64::from(value) * (f64::from(size) / f64::from(longest))).round();
    scaled.max(1.0) as u32
}

/// Scale the longest edge to `size`, preserving the aspect ratio.
///
/// A zero-sized image (still decoding, or a broken source) yields `None` rather
/// than a 0x0 plan.
pub fn plan_download(
    natural_width: u32,
    natural_height: u32,
    size: u32,
    format: &str,
    quality_percent: u32,
) -> Option<DownloadPlan> {
    let longest = natural_width.max(natural_height);
    if longest == 0 || size == 0 {
        return None;
    }
    let lossy = format != "image/png";
    Some(DownloadPlan {
        width: scale_edge(natural_width, size, longest),
        height: scale_edge(natural_height, size, longest),
        quality: if lossy { (quality_percent.min(100) as f32) / 100.0 } else { 1.0 },
        estimates_size: lossy,
    })
}

/// The size label next to the export. 1024-based: this describes a file, not a
/// marketing number.
pub fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    if bytes < 1024 * 1024 {
        return format!("{:.1} KB", bytes as f64 / 1024.0);
    }
    format!("{:.2} MB", bytes as f64 / (1024.0 * 1024.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kestrel_proto::event::{AssetView, ToolCallRecord};
    use kestrel_proto::{TaskKind, TaskStatus};

    fn stored(id: i64) -> MessageView {
        MessageView {
            id,
            role: "assistant".to_string(),
            content: "画好了".to_string(),
            reasoning: None,
            tool_calls: None,
            mode: Some(Mode::Image),
            created_at: 1,
            assets: Vec::new(),
        }
    }

    #[test]
    fn a_message_the_shell_already_painted_is_not_painted_again() {
        let mut stream = Stream::new();
        stream.note_rendered(7);
        assert!(stream.on_event(&ConversationEvent::UserMessage { message: stored(7) }).is_empty());
        assert!(stream.is_rendered(7));

        let paints = stream.on_event(&ConversationEvent::UserMessage { message: stored(8) });
        assert_eq!(paints.len(), 1);
        assert!(matches!(&paints[0], Paint::RenderMessage { message } if message.id == 8));
        assert!(
            stream.on_event(&ConversationEvent::MessageAdded { message: stored(8) }).is_empty(),
            "the same message on another event name is still the same message"
        );
    }

    #[test]
    fn opening_a_turn_twice_paints_once_but_deltas_always_pass_through() {
        let mut stream = Stream::new();
        let start = ConversationEvent::TurnStarted { message_id: 9, mode: Some(Mode::Image) };
        assert_eq!(
            stream.on_event(&start),
            vec![Paint::OpenTurn { message_id: 9, mode: Some(Mode::Image) }]
        );
        assert!(stream.on_event(&start).is_empty(), "a replay must not open a second bubble");

        let delta = ConversationEvent::Text { message_id: 9, delta: "画".to_string() };
        assert_eq!(stream.on_event(&delta).len(), 1);
        assert_eq!(
            stream.on_event(&delta).len(),
            1,
            "the journal replays deltas the page has not seen; filtering here would eat them"
        );
    }

    #[test]
    fn asset_and_task_events_carry_the_names_the_page_keys_on() {
        let mut stream = Stream::new();
        assert_eq!(
            stream.on_event(&ConversationEvent::Asset {
                message_id: 3,
                id: 12,
                kind: TaskKind::Video,
                url: "/api/v1/assets/12/download".to_string(),
                mime: "video/mp4".to_string(),
                bytes: 8,
            }),
            vec![Paint::Asset {
                message_id: 3,
                kind: "video".to_string(),
                url: "/api/v1/assets/12/download".to_string()
            }]
        );

        assert_eq!(
            stream.on_event(&ConversationEvent::TaskUpdated {
                task_id: 4,
                kind: TaskKind::Video,
                status: TaskStatus::Running,
                error: None,
            }),
            vec![Paint::TaskUpdated {
                task_id: 4,
                kind: "video".to_string(),
                status: "running".to_string(),
                error: None
            }]
        );
    }

    #[test]
    fn a_resync_cursor_reloads_the_page_and_nothing_else_does() {
        assert_eq!(Stream::paint_for_name("resync"), Some(Paint::Reload));
        assert_eq!(Stream::paint_for_name("connected"), None);
        assert_eq!(Stream::paint_for_name("text"), None);
    }

    #[test]
    fn the_stored_mode_is_validated_not_defaulted() {
        assert_eq!(stored_mode(Some("video")), Some(Mode::Video));
        assert_eq!(stored_mode(Some("nonsense")), None);
        assert_eq!(stored_mode(Some("Video")), None, "the selector stores the wire name verbatim");
        assert_eq!(stored_mode(None), None);
    }

    #[test]
    fn the_longest_edge_is_scaled_and_the_other_keeps_its_ratio() {
        let plan = plan_download(2000, 1000, 1000, "image/jpeg", 82).expect("plans");
        assert_eq!((plan.width, plan.height), (1000, 500));
        assert!((plan.quality - 0.82).abs() < 1e-6);
        assert!(plan.estimates_size);
    }

    #[test]
    fn a_short_edge_never_rounds_to_zero() {
        let plan = plan_download(1000, 10, 100, "image/jpeg", 90).expect("plans");
        assert_eq!((plan.width, plan.height), (100, 1), "10 px scaled by 0.1 would round to 0");
    }

    #[test]
    fn png_is_not_recompressed_and_a_broken_image_is_not_planned() {
        let plan = plan_download(4, 2, 8, "image/png", 30).expect("plans");
        assert_eq!(plan.quality, 1.0, "PNG ignores the quality argument");
        assert!(!plan.estimates_size);
        // The longest edge (4) becomes the requested 8, so the ratio is exactly 2.
        assert_eq!((plan.width, plan.height), (8, 4));

        assert!(plan_download(0, 0, 100, "image/jpeg", 80).is_none(), "still decoding");
        assert!(plan_download(100, 100, 0, "image/jpeg", 80).is_none(), "no size chosen");
    }

    #[test]
    fn byte_labels_switch_units_at_1024() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_bytes(1024 * 1024), "1.00 MB");
        assert_eq!(format_bytes(2_621_440), "2.50 MB");
    }

    /// The page parses what the server wrote, from the same type. This is the test
    /// that the old hand-restated client could not have: it would have kept
    /// compiling after a server-side rename.
    #[test]
    fn a_full_message_decodes_from_the_published_json() {
        let message = MessageView {
            id: 5,
            role: "assistant".to_string(),
            content: "看这张".to_string(),
            reasoning: Some("先想一下".to_string()),
            tool_calls: Some(vec![ToolCallRecord {
                name: "generate_image".to_string(),
                arguments: serde_json::json!({ "prompt": "红隼" }),
                ok: true,
                summary: "已生成".to_string(),
            }]),
            mode: Some(Mode::Auto),
            created_at: 1_700_000_000,
            assets: vec![AssetView {
                id: 9,
                kind: TaskKind::Image,
                url: "/api/v1/assets/9/download".to_string(),
                mime: "image/png".to_string(),
                bytes: 4,
            }],
        };
        let raw = serde_json::to_string(&ConversationEvent::UserMessage {
            message: message.clone(),
        })
        .expect("serialises");
        let parsed: ConversationEvent = serde_json::from_str(&raw).expect("the client parses it");
        assert_eq!(parsed, ConversationEvent::UserMessage { message });
        assert!(raw.contains("\"createdAt\""), "camelCase is the published shape: {raw}");
        assert!(raw.contains("\"toolCalls\""));
        assert!(raw.contains("\"type\":\"user_message\""));
    }
}
