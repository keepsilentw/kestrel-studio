pub mod service;
pub mod worker;

// Both halves of a task's wire shape live in `kestrel-proto`: the kind tag rides
// the asset frame, and the status rides `task_updated`, which the page paints a
// label from. This tree keeps importing them as `crate::task::{TaskKind, TaskStatus}`.
pub use kestrel_proto::{TaskKind, TaskStatus};

/// A task in one of these states is still owned by the background worker.
pub const ACTIVE_STATUSES: [TaskStatus; 2] = [TaskStatus::Queued, TaskStatus::Running];

/// How long a claim on a render holds against the *other* backend. A sweep renews
/// its own claim every tick regardless of this window, so the value only decides how
/// long a task stays with a stack that stopped advancing it — one crashed poll plus
/// the longest mirror download. Five minutes is comfortably inside the 20-minute age
/// limit the worker already enforces, so a lease that is never renewed still ends in
/// a terminal state rather than an orphan. `nest/server/src/task/task.worker.ts`
/// carries the same number: both stacks read and write one table.
pub const CLAIM_LEASE_MS: i64 = 5 * 60 * 1000;
