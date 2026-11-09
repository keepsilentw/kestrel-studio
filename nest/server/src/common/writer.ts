/**
 * Which backend wrote a row.
 *
 * Two backends (`nest/server`, `rust/server`) share one SQLite file and one
 * `storage/` directory on purpose — docs/roadmap.md §4.5 made the pair permanent.
 * Three writes have no process boundary around them, so they carry a tag in the data
 * instead: the boot recovery of abandoned turns, the task worker's claim on a render,
 * and the journal's `seq`. See docs/architecture.md §13.2.
 *
 * The identity is **per backend stack, not per process**, and it has to survive a
 * restart: a new process of the same stack is exactly who is meant to clean up what
 * the previous one left mid-turn. Each stack runs one process against the database —
 * the entry is pinned per subdomain (docs/deployment.md §1.1), which is also why the
 * per-conversation turn lock can stay in-process memory.
 *
 * `rust/server/src/common/writer.rs` is the counterpart. The two strings are the
 * whole protocol: changing one without the other makes the pair treat each other's
 * rows as foreign.
 */
export const WRITER_ID = 'nest';
