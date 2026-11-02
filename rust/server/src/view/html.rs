//! Escaping and the markdown subset, re-exported from `kestrel-md`.
//!
//! The code lives in `md/` because the browser client compiles the same renderer
//! to WASM: one implementation decides how a turn looks, whether it is painted by
//! SSR on first byte or re-rendered in the page as it streams. Templates and tests
//! keep reaching it as `super::html::{escape, markdown}`.
//!
//! Tests for the subset live in `tests.rs`
//! (`the_markdown_subset_matches_what_the_assistant_actually_writes`), covering the
//! two properties that justify a subset over a full engine: raw HTML is escaped
//! rather than forwarded, and escaping is applied exactly once.

pub use kestrel_md::{escape, markdown};
