use std::path::{Component, Path, PathBuf};

use axum::extract::{Path as UrlPath, State};
use axum::http::header::{self, ETAG, IF_NONE_MATCH, LAST_MODIFIED};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::error::AppError;
use crate::state::AppState;

/// Serves the Vite output and the favicon out of `public/`, the way
/// `app.useStaticAssets(..., { index: false })` did: no directory index, no
/// rewrite, only real files.
///
/// `tower-http`'s `fs` feature is unavailable in this build (it needs
/// `http-range-header`, which is not in the local registry), so the handler is
/// here instead. Asset names are stable (`assets/main.js`), which the templates
/// link to directly.
pub async fn serve(
    State(state): State<AppState>,
    UrlPath(requested): UrlPath<String>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    serve_at(&state, &requested, &headers).await
}

/// Registered as the router fallback: anything the API routes do not claim is a
/// file request, which is how `useStaticAssets` behaved for `/logo.svg` and the
/// Vite output. A missing file answers 404 rather than rendering HTML.
pub async fn fallback(
    State(state): State<AppState>,
    uri: axum::http::Uri,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    serve_at(&state, uri.path().trim_start_matches('/'), &headers).await
}

async fn serve_at(state: &AppState, requested: &str, headers: &HeaderMap) -> Result<Response, AppError> {
    let root = state.public_dir();
    let path = resolve_under(&root, requested)?;

    let meta = match tokio::fs::metadata(&path).await {
        Ok(meta) if meta.is_file() => meta,
        _ => return Ok(not_found()),
    };

    let modified = meta
        .modified()
        .ok()
        .map(|time| httpdate_like(time))
        .unwrap_or_else(|| "Thu, 01 Jan 1970 00:00:00 GMT".to_string());
    let etag = format!(
        "W/\"{}-{}\"",
        meta
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|since| since.as_millis())
            .unwrap_or(0),
        meta.len()
    );

    if headers
        .get(IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == etag)
    {
        return Ok((StatusCode::NOT_MODIFIED, [(ETAG, etag)]).into_response());
    }

    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|error| AppError::internal(anyhow::anyhow!("reading {}: {error}", path.display())))?;
    let mime = mime_guess::from_path(&path)
        .first_raw()
        .unwrap_or("application/octet-stream")
        .to_string();

    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, mime),
            (header::CACHE_CONTROL, "no-cache".to_string()),
            (LAST_MODIFIED, modified),
            (ETAG, etag),
        ],
        bytes,
    )
        .into_response())
}

/// Joins a URL path under `root`, refusing anything that escapes it.
fn resolve_under(root: &Path, requested: &str) -> Result<PathBuf, AppError> {
    let mut path = root.to_path_buf();
    for segment in requested.split('/') {
        match segment {
            "" | "." => continue,
            ".." => {
                return Err(AppError::bad_request("非法路径"));
            }
            other => {
                if other.contains('\0') {
                    return Err(AppError::bad_request("非法路径"));
                }
                path.push(other);
            }
        }
    }

    if !path.starts_with(root) {
        return Err(AppError::bad_request("非法路径"));
    }
    // Only ordinary components: symlinks inside `public/` are not part of the
    // build, and rejecting them keeps the check independent of the filesystem.
    if path
        .strip_prefix(root)
        .unwrap_or(Path::new(""))
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(AppError::bad_request("非法路径"));
    }
    Ok(path)
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, "Not Found").into_response()
}

/// `Last-Modified` wants an RFC 7231 date; chrono is already a dependency.
fn httpdate_like(time: std::time::SystemTime) -> String {
    let datetime = chrono::DateTime::<chrono::Utc>::from(time);
    datetime.format("%a, %d %b %Y %H:%M:%S GMT").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traversal_is_refused_and_plain_paths_are_joined() {
        let root = Path::new("/srv/public");
        assert_eq!(
            resolve_under(root, "assets/main.js").expect("joined"),
            PathBuf::from("/srv/public/assets/main.js")
        );
        assert_eq!(
            resolve_under(root, "/assets//./main.css").expect("normalised"),
            PathBuf::from("/srv/public/assets/main.css")
        );
        for evil in ["../etc/passwd", "assets/../../etc/passwd", "a/./b/../../../x"] {
            assert!(resolve_under(root, evil).is_err(), "{evil}");
        }
    }
}
