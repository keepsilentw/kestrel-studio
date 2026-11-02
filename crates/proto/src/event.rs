//! The conversation event stream's payload, in one place.
//!
//! This is the same move that [`crate::ClientFrame`] / [`crate::ServerFrame`] made
//! for the voice socket, applied to the bigger channel: the browser used to
//! restate these shapes by hand (`readString(data, 'messageId')` in
//! `web/scripts/main.ts`), so a renamed field on the server was invisible until
//! the page silently stopped painting. Now the server publishes these types and
//! the client parses them, and a rename breaks the side that consumes it.
//!
//! What lives here is deliberately only the **wire**: data plus the naming every
//! surface needs. The server's agent logic over these values — which tools a mode
//! advertises, the system prompt a mode builds — stays in `rust/src/agent/mode.rs`
//! behind an extension trait, because the browser has no business knowing either.
//!
//! Field names are camelCase because that is what the journal already stores: the
//! `payload` column is replayed byte-for-byte (docs/architecture.md §4.2), so this
//! is a description of the existing wire, not a new choice.

use serde::{Deserialize, Serialize};

use crate::{TaskKind, TaskStatus};

/// A turn's capability mode. The selector is per turn, not per conversation: one
/// conversation can chat, then draw, then animate, and each turn is stored with
/// the mode it ran in so history renders correctly.
///
/// `auto` exposes every tool and lets the model route itself; the explicit modes
/// narrow the tool set so a turn cannot silently spend money on a generation the
/// user did not ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Auto,
    Chat,
    Image,
    Video,
}

pub const DEFAULT_MODE: Mode = Mode::Auto;
pub const MODES: [Mode; 4] = [Mode::Auto, Mode::Chat, Mode::Image, Mode::Video];

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Chat => "chat",
            Self::Image => "image",
            Self::Video => "video",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "智能",
            Self::Chat => "对话",
            Self::Image => "图片",
            Self::Video => "视频",
        }
    }

    /// What the page's selector sends. Unknown and missing values fall back to
    /// `auto`, which is the only mode that cannot under-explain itself to the user.
    pub fn parse(value: Option<&str>) -> Mode {
        let Some(raw) = value else {
            return DEFAULT_MODE;
        };
        let normalized = raw.trim().to_lowercase();
        MODES
            .into_iter()
            .find(|mode| mode.as_str() == normalized)
            .unwrap_or(DEFAULT_MODE)
    }

    /// Reading the `mode` column back. Unlike [`Mode::parse`] there is no default:
    /// null means "no mode" (a voice turn), and a value this build does not know
    /// must not be relabelled as one it does.
    pub fn from_storage(raw: Option<&str>) -> Option<Mode> {
        let raw = raw?;
        MODES.into_iter().find(|mode| mode.as_str() == raw)
    }
}

/// One tool round-trip, stored as JSON on the assistant message so a reload can
/// render the turn the way it ran.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallRecord {
    pub name: String,
    pub arguments: serde_json::Value,
    pub ok: bool,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetView {
    pub id: i64,
    /// `image` or `video`.
    pub kind: TaskKind,
    /// Download endpoint, not the on-disk path.
    pub url: String,
    pub mime: String,
    pub bytes: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageView {
    pub id: i64,
    pub role: String,
    pub content: String,
    pub reasoning: Option<String>,
    pub tool_calls: Option<Vec<ToolCallRecord>>,
    pub mode: Option<Mode>,
    pub created_at: i64,
    pub assets: Vec<AssetView>,
}

/// One journaled event on a conversation stream. The `type` discriminant is the
/// same name the SSE frame carries, and the whole object is what the frame's
/// `data:` holds — stored verbatim so a replayed frame is byte-equivalent to the
/// live one. (Frame names: `rust/src/sse.rs`.)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ConversationEvent {
    #[serde(rename = "user_message")]
    UserMessage { message: MessageView },
    #[serde(rename = "turn_started")]
    TurnStarted {
        #[serde(rename = "messageId")]
        message_id: i64,
        mode: Option<Mode>,
    },
    #[serde(rename = "reasoning")]
    Reasoning {
        #[serde(rename = "messageId")]
        message_id: i64,
        delta: String,
    },
    #[serde(rename = "text")]
    Text {
        #[serde(rename = "messageId")]
        message_id: i64,
        delta: String,
    },
    #[serde(rename = "tool_call")]
    ToolCall {
        #[serde(rename = "messageId")]
        message_id: i64,
        name: String,
        arguments: serde_json::Value,
        status: String,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        #[serde(rename = "messageId")]
        message_id: i64,
        name: String,
        ok: bool,
        summary: String,
    },
    #[serde(rename = "asset")]
    Asset {
        #[serde(rename = "messageId")]
        message_id: i64,
        // Flat, exactly as `AssetView` serialises: an internally tagged enum
        // cannot carry a flattened struct inside its variant.
        id: i64,
        kind: TaskKind,
        url: String,
        mime: String,
        bytes: i64,
    },
    #[serde(rename = "turn_done")]
    TurnDone {
        #[serde(rename = "messageId")]
        message_id: i64,
    },
    #[serde(rename = "turn_error")]
    TurnError {
        #[serde(rename = "messageId")]
        message_id: i64,
        message: String,
    },
    #[serde(rename = "task_updated")]
    TaskUpdated {
        #[serde(rename = "taskId")]
        task_id: i64,
        kind: TaskKind,
        status: TaskStatus,
        error: Option<String>,
    },
    #[serde(rename = "message_added")]
    MessageAdded { message: MessageView },
}

impl ConversationEvent {
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::UserMessage { .. } => "user_message",
            Self::TurnStarted { .. } => "turn_started",
            Self::Reasoning { .. } => "reasoning",
            Self::Text { .. } => "text",
            Self::ToolCall { .. } => "tool_call",
            Self::ToolResult { .. } => "tool_result",
            Self::Asset { .. } => "asset",
            Self::TurnDone { .. } => "turn_done",
            Self::TurnError { .. } => "turn_error",
            Self::TaskUpdated { .. } => "task_updated",
            Self::MessageAdded { .. } => "message_added",
        }
    }

    /// The message a journal row should hang off, for mid-turn catch-up lookups.
    pub fn message_id_of(&self) -> Option<i64> {
        match self {
            Self::UserMessage { message } | Self::MessageAdded { message } => Some(message.id),
            // Task events belong to the conversation, not to one message.
            Self::TaskUpdated { .. } => None,
            Self::TurnStarted { message_id, .. }
            | Self::Reasoning { message_id, .. }
            | Self::Text { message_id, .. }
            | Self::ToolCall { message_id, .. }
            | Self::ToolResult { message_id, .. }
            | Self::Asset { message_id, .. }
            | Self::TurnDone { message_id }
            | Self::TurnError { message_id, .. } => Some(*message_id),
        }
    }
}
