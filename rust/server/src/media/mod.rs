pub mod asset_path;
pub mod dims;
pub mod service;

use crate::task::TaskKind;

/// A generated file that has been mirrored into `storage/`.
#[derive(Debug, Clone)]
pub struct StoredAsset {
    /// `image` or `video`. The same vocabulary the task table uses, because the
    /// asset is the result of a task.
    pub kind: TaskKind,
    /// Absolute path at write time — only `asset_path::stored_asset_name` goes
    /// into the row.
    pub file_path: String,
    /// The provider's short-lived URL. Audit only, never a durable reference.
    pub source_url: Option<String>,
    pub mime: String,
    pub bytes: i64,
    /// Pixel dimensions read from the bytes themselves — see `dims`. `None` for a
    /// video (this reads image headers only) and for an image in a format whose
    /// header we cannot parse; both leave the `width`/`height` attributes off the
    /// `<img>` rather than inventing a box.
    pub width: Option<i64>,
    pub height: Option<i64>,
}
