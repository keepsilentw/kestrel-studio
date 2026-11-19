use std::path::{Component, Path, PathBuf};

use axum::extract::State;
use axum::http::header::{self, ETAG, IF_NONE_MATCH, LAST_MODIFIED};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::error::AppError;
use crate::state::AppState;

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

async fn serve_at(
    state: &AppState,
    requested: &str,
    headers: &HeaderMap,
) -> Result<Response, AppError> {
    let root = state.public_dir();
    let raw_path = resolve_under(&root, requested)?;
    let raw_meta = match tokio::fs::metadata(&raw_path).await {
        Ok(meta) if meta.is_file() => meta,
        _ => return Ok(not_found()),
    };

    // openresty gzips JS/CSS but not `application/wasm`, so `make build-wasm`
    // leaves a `.gz` next to the wasm. Serve that copy when the client accepts it;
    // the URL and the loader stay unchanged.
    let compressed = requested.ends_with(".wasm") && accepts_gzip(headers);
    let gzip_path = compressed.then(|| gzip_variant(&raw_path));
    let (path, meta, mime, encoding) = match gzip_path {
        Some(gzip) => match tokio::fs::metadata(&gzip).await {
            Ok(meta) if meta.is_file() => {
                (gzip, meta, "application/wasm".to_string(), Some("gzip"))
            }
            _ => (raw_path.clone(), raw_meta, mime_of(&raw_path), None),
        },
        None => (raw_path.clone(), raw_meta, mime_of(&raw_path), None),
    };

    let modified = meta
        .modified()
        .ok()
        .map(httpdate_like)
        .unwrap_or_else(|| "Thu, 01 Jan 1970 00:00:00 GMT".to_string());
    let etag = format!(
        "W/\"{}-{}\"",
        meta.modified()
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
        let mut response = HeaderMap::new();
        response.insert(ETAG, header_value(&etag));
        if requested.ends_with(".wasm") {
            response.insert(header::VARY, header_value("Accept-Encoding"));
        }
        return Ok((StatusCode::NOT_MODIFIED, response).into_response());
    }

    let bytes = tokio::fs::read(&path).await.map_err(|error| {
        AppError::internal(anyhow::anyhow!("reading {}: {error}", path.display()))
    })?;
    let mut response = HeaderMap::new();
    response.insert(header::CONTENT_TYPE, header_value(&mime));
    response.insert(header::CACHE_CONTROL, header_value("no-cache"));
    response.insert(LAST_MODIFIED, header_value(&modified));
    response.insert(ETAG, header_value(&etag));
    if let Some(encoding) = encoding {
        response.insert(header::CONTENT_ENCODING, header_value(encoding));
    }
    if requested.ends_with(".wasm") {
        response.insert(header::VARY, header_value("Accept-Encoding"));
    }

    Ok((StatusCode::OK, response, bytes).into_response())
}

fn mime_of(path: &Path) -> String {
    mime_guess::from_path(path)
        .first_raw()
        .unwrap_or("application/octet-stream")
        .to_string()
}

fn gzip_variant(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".gz");
    PathBuf::from(name)
}

/// `Accept-Encoding` is only consulted for the wasm file. A token with `q=0` is a
/// refusal, not an acceptance.
fn accepts_gzip(headers: &HeaderMap) -> bool {
    let Some(raw) = headers
        .get(header::ACCEPT_ENCODING)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    raw.split(',').any(|part| {
        let mut pieces = part.split(';');
        let token = pieces.next().unwrap_or("").trim();
        if !token.eq_ignore_ascii_case("gzip") {
            return false;
        }
        !pieces.any(|piece| {
            let piece = piece.trim();
            piece.eq_ignore_ascii_case("q=0") || piece.eq_ignore_ascii_case("q=0.0")
        })
    })
}

fn header_value(value: &str) -> axum::http::HeaderValue {
    axum::http::HeaderValue::from_str(value)
        .unwrap_or_else(|_| axum::http::HeaderValue::from_static(""))
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
        for evil in [
            "../etc/passwd",
            "assets/../../etc/passwd",
            "a/./b/../../../x",
        ] {
            assert!(resolve_under(root, evil).is_err(), "{evil}");
        }
    }

    #[test]
    fn gzip_is_only_accepted_when_asked_for() {
        let mut headers = HeaderMap::new();
        assert!(!accepts_gzip(&headers));

        headers.insert(
            header::ACCEPT_ENCODING,
            axum::http::HeaderValue::from_static("gzip, deflate"),
        );
        assert!(accepts_gzip(&headers));

        headers.insert(
            header::ACCEPT_ENCODING,
            axum::http::HeaderValue::from_static("gzip;q=0"),
        );
        assert!(!accepts_gzip(&headers));
    }

    #[test]
    fn the_gzip_variant_keeps_the_dot_in_the_name() {
        assert_eq!(
            gzip_variant(Path::new("/srv/public/wasm/a.wasm")),
            PathBuf::from("/srv/public/wasm/a.wasm.gz")
        );
    }
}
