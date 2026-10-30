pub mod service;
pub mod worker;

// Both halves of a task's wire shape live in `kestrel-proto`: the kind tag rides
// the asset frame, and the status rides `task_updated`, which the page paints a
// label from. This tree keeps importing them as `crate::task::{TaskKind, TaskStatus}`.
pub use kestrel_proto::{TaskKind, TaskStatus};

/// A task in one of these states is still owned by the background worker.
pub const ACTIVE_STATUSES: [TaskStatus; 2] = [TaskStatus::Queued, TaskStatus::Running];
