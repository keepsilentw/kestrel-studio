use std::path::{Path, PathBuf};

use crate::config::AppConfig;

/// What goes into `assets.file_path`, and how it comes back out.
///
/// The column holds the file **name**, not a path. An absolute path baked in at
/// write time stops being true the moment the project moves or the storage
/// directory changes, and this bit once already: every asset written before the
/// project left `playground/` kept pointing at the old directory, so the download
/// endpoint answered 404 for the owner too. Where the bytes live is a property of
/// the running instance, so it belongs at read time, not in the row.
///
/// Rows written before the change hold an absolute path. One that is still there
/// is honoured as-is, so an existing deployment keeps serving without a migration.
pub fn stored_asset_name(file_path: &str) -> String {
    basename(file_path)
}

/// The file a stored value points at right now.
pub fn resolve_asset_path(config: &AppConfig, stored: &str) -> PathBuf {
    let candidate = Path::new(stored);
    if candidate.is_absolute() && candidate.exists() {
        return candidate.to_path_buf();
    }
    absolute(&config.storage_dir).join(basename(stored))
}

/// Deletes mirrored bytes, best effort: the rows are already gone by the time
/// this runs, and a file that is missing or unreadable is nothing left to reclaim.
/// Anything resolving outside `STORAGE_DIR` is skipped rather than followed — only
/// rows written before the column held a bare name can point there.
pub async fn remove_asset_files(config: &AppConfig, stored_names: &[String]) {
    let root = absolute(&config.storage_dir);
    for stored in stored_names {
        let file_path = resolve_asset_path(config, stored);
        if !starts_under(&file_path, &root) {
            continue;
        }
        if let Err(error) = tokio::fs::remove_file(&file_path).await {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(%error, path = %file_path.display(), "asset file left behind");
            }
        }
    }
}

fn basename(value: &str) -> String {
    Path::new(value)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// `path.resolve()` from the Nest version: make a relative value absolute against
/// the process working directory, without touching the filesystem.
pub(crate) fn absolute(value: &str) -> PathBuf {
    let path = Path::new(value);
    if path.is_absolute() {
        return normalise(path);
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    normalise(&cwd.join(path))
}

fn normalise(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn starts_under(file_path: &Path, root: &Path) -> bool {
    file_path
        .strip_prefix(root)
        .map(|rest| !rest.as_os_str().is_empty())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stored_value_is_a_bare_name_and_resolves_under_storage_dir() {
        let config = AppConfig::test("/tmp/kestrel-test-storage", "https://example.test");
        assert_eq!(stored_asset_name("/a/b/c.png"), "c.png");
        assert_eq!(stored_asset_name("/a/b/"), "b");
        let resolved = resolve_asset_path(&config, "c.png");
        assert_eq!(resolved, PathBuf::from("/tmp/kestrel-test-storage/c.png"));
        assert!(starts_under(&resolved, Path::new("/tmp/kestrel-test-storage")));
        assert!(!starts_under(
            &PathBuf::from("/tmp/other/x.png"),
            Path::new("/tmp/kestrel-test-storage")
        ));
    }

    #[test]
    fn a_legacy_absolute_path_that_still_exists_is_honoured() {
        let here = std::env::current_dir().expect("cwd").join("Cargo.toml");
        assert!(here.exists(), "the probe file is this crate's manifest");
        let config = AppConfig::test("/tmp/kestrel-test-storage", "https://example.test");
        assert_eq!(
            resolve_asset_path(&config, &here.to_string_lossy()),
            here,
            "no migration, no 404 for rows written before the change"
        );
        // The same value pointing at a file that is gone falls back to storage.
        let gone = std::env::temp_dir().join("kestrel-definitely-not-here.png");
        assert_eq!(
            resolve_asset_path(&config, &gone.to_string_lossy()),
            PathBuf::from("/tmp/kestrel-test-storage/kestrel-definitely-not-here.png")
        );
    }

    #[test]
    fn relative_storage_dirs_are_resolved_once_and_stay_consistent() {
        let config = AppConfig::test("storage", "https://example.test");
        let resolved = resolve_asset_path(&config, "frame.png");
        assert!(resolved.is_absolute());
        assert_eq!(resolved.file_name().map(|name| name.to_str()).flatten(), Some("frame.png"));
        assert!(absolute("./storage").ends_with("storage"));
        assert_eq!(absolute("a/../b"), absolute("b"), "dot segments collapse");
    }

    #[tokio::test]
    async fn removing_a_missing_or_escaping_file_is_not_an_error() {
        let config = AppConfig::test("/tmp/kestrel-test-storage", "https://example.test");
        remove_asset_files(
            &config,
            &["ghost.png".to_string(), "../outside.png".to_string()],
        )
        .await;
    }
}
