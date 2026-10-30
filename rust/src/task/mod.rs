pub mod service;
pub mod worker;

use serde::{Deserialize, Serialize};

// The kind tag lives in `kestrel-proto` because the asset frame carries it to
// the browser, and the client now compiles the same type. Everything in this
// tree keeps importing `crate::task::TaskKind`.
pub use kestrel_proto::TaskKind;

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
