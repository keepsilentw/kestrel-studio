pub mod service;
pub mod worker;

use serde::{Deserialize, Serialize};

/// Async provider jobs (video today). A video render takes minutes, which does
/// not fit inside an SSE turn: the tool submits, returns immediately, and the
/// background worker polls the provider, mirrors the bytes at the moment of
/// success (provider URLs expire) and appends the result as a new assistant
/// message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    Image,
    Video,
}

impl TaskKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Video => "video",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
}

impl TaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }

    pub fn parse(raw: &str) -> TaskStatus {
        match raw {
            "running" => Self::Running,
            "succeeded" => Self::Succeeded,
            "failed" => Self::Failed,
            _ => Self::Queued,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => "排队中",
            Self::Running => "渲染中",
            Self::Succeeded => "已完成",
            Self::Failed => "失败",
        }
    }
}

/// A task in one of these states is still owned by the background worker.
pub const ACTIVE_STATUSES: [TaskStatus; 2] = [TaskStatus::Queued, TaskStatus::Running];
