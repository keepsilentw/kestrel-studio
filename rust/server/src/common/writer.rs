//! Which backend wrote a row.
//!
//! Two backends (`rust/server`, `nest/server`) share one SQLite file and one
//! `storage/` directory on purpose — docs/roadmap.md §4.5 made the pair permanent.
//! Three writes have no process boundary around them, so they need a tag in the
//! data instead: the boot recovery that ends abandoned turns, the task worker's
//! claim on a render, and the journal's `seq`. See docs/architecture.md §13.2.
//!
//! The identity is **per backend stack, not per process**, and it must survive a
//! restart: a new process of the same stack is exactly who is supposed to clean up
//! what the previous one left mid-turn. Each stack runs one process against the
//! database — the entry is pinned per subdomain (docs/deployment.md §1.1), which is
//! also why the per-conversation turn lock staying in-process is tolerable.

/// The value written to `messages.owner` and `generation_tasks.claimed_by`.
/// `nest/server/src/common/writer.ts` is the counterpart; the two strings are the
/// whole protocol, so changing either one means changing the other in the same
/// commit, or the pair starts treating each other's rows as foreign.
pub const WRITER_ID: &str = "rust";
