use std::sync::Arc;

use serde_json::Value;

use crate::config::AppConfig;
use crate::error::AppError;
use crate::task::TaskKind;

use super::StoredAsset;

pub const DEFAULT_IMAGE_MODEL: &str = "wan2.7-image";
const IMAGE_MODELS: [&str; 2] = ["wan2.7-image", "wan2.7-image-pro"];

const MIME_BY_EXTENSION: [(&str, &str); 7] = [
    (".png", "image/png"),
    (".jpg", "image/jpeg"),
    (".jpeg", "image/jpeg"),
    (".webp", "image/webp"),
    (".mp4", "video/mp4"),
    (".mov", "video/quicktime"),
    (".webm", "video/webm"),
];

fn default_extension(kind: TaskKind) -> &'static str {
    match kind {
        TaskKind::Image => ".png",
        TaskKind::Video => ".mp4",
    }
}

fn allowed_extensions(kind: TaskKind) -> &'static [&'static str] {
    match kind {
        TaskKind::Image => &[".png", ".jpg", ".jpeg", ".webp"],
        TaskKind::Video => &[".mp4", ".mov", ".webm"],
    }
}

/// Rejects a model this build has no request shape for. Cheaper than letting the
/// provider answer with a 400 minutes later.
pub fn assert_image_model(model: &str) -> Result<(), String> {
    if IMAGE_MODELS.contains(&model) {
        Ok(())
    } else {
        Err(format!(
            "Unsupported image model \"{model}\". Supported: {}",
            IMAGE_MODELS.join(", ")
        ))
    }
}

/// Normalized provider task state. Everything unrecognized counts as terminal
/// failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoTaskState {
    Pending,
    Running,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone)]
pub struct VideoTaskSnapshot {
    pub state: VideoTaskState,
    /// The provider's own status string, kept for messages and diagnostics.
    pub raw_status: String,
    pub video_url: Option<String>,
    pub message: Option<String>,
}

/// Exported for tests: the SUCCEEDED-without-URL rule is worth pinning.
pub fn normalize_task_state(raw_status: &str, has_video: bool) -> VideoTaskState {
    match raw_status {
        "PENDING" => VideoTaskState::Pending,
        "RUNNING" => VideoTaskState::Running,
        // A SUCCEEDED task without a URL has nothing to hand back; treat it as a
        // failure rather than polling it forever.
        "SUCCEEDED" if has_video => VideoTaskState::Succeeded,
        _ => VideoTaskState::Failed,
    }
}

/// One slice of the Bailian surface: four different paths under one base URL.
/// Chat goes to `/compatible-mode/v1/responses` (see `bailian::responses`), so do
/// not mix it in here.
pub struct MediaService {
    http: reqwest::Client,
    config: Arc<AppConfig>,
}

impl MediaService {
    pub fn new(http: reqwest::Client, config: Arc<AppConfig>) -> Self {
        Self { http, config }
    }

    fn bearer(api_key: &str) -> reqwest::header::HeaderValue {
        reqwest::header::HeaderValue::from_str(&format!("Bearer {api_key}"))
            .unwrap_or_else(|_| reqwest::header::HeaderValue::from_static("Bearer "))
    }

    /// Calls the multimodal-generation endpoint, which returns image URLs. Same
    /// path and payload shape as `playground/bailian-media-mcp`.
    pub async fn generate_images(
        &self,
        api_key: &str,
        prompt: &str,
        model: &str,
        size: &str,
        count: u32,
    ) -> Result<Vec<String>, AppError> {
        assert_image_model(model).map_err(AppError::bad_request)?;
        let url = format!(
            "{}/api/v1/services/aigc/multimodal-generation/generation",
            self.config.bailian.base_url
        );
        let body = serde_json::json!({
            "model": model,
            "input": { "messages": [{ "role": "user", "content": [{ "text": prompt }] }] },
            "parameters": { "size": size, "n": count },
        });

        let payload = self
            .post_json(&url, api_key, &body, "Image generation")
            .await?;

        let content = payload
            .pointer("/output/choices/0/message/content")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let urls: Vec<String> = content
            .iter()
            .filter(|part| part.get("type").and_then(Value::as_str) == Some("image"))
            .filter_map(|part| part.get("image").and_then(Value::as_str))
            .filter(|value| !value.is_empty())
            .map(|value| value.to_string())
            .collect();

        if urls.is_empty() {
            return Err(AppError::internal(anyhow::anyhow!(
                "Bailian returned no image for this request."
            )));
        }
        Ok(urls)
    }

    /// Submits an async video job and returns the provider task id. The submit
    /// call only accepts the job; the render itself is collected by
    /// `query_video_task`.
    pub async fn submit_video(
        &self,
        api_key: &str,
        prompt: &str,
        model: &str,
        size: &str,
        duration: u32,
        first_frame_url: Option<&str>,
    ) -> Result<String, AppError> {
        let url = format!(
            "{}/api/v1/services/aigc/video-generation/video-synthesis",
            self.config.bailian.base_url
        );
        let mut input = serde_json::json!({ "prompt": prompt });
        if let Some(frame) = first_frame_url {
            // Image-to-video: the provider fetches this itself, so it has to be the
            // signed, publicly reachable frame URL, not a localhost path.
            input["media"] = serde_json::json!([{ "type": "first_frame", "url": frame }]);
        }
        let body = serde_json::json!({
            "model": model,
            "input": input,
            "parameters": { "size": size, "duration": duration },
        });

        // Without this header the endpoint runs synchronously and the request would
        // hang for the whole render.
        let response = self
            .http
            .post(&url)
            .header(reqwest::header::AUTHORIZATION, Self::bearer(api_key))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header("X-DashScope-Async", "enable")
            .json(&body)
            .send()
            .await
            .map_err(|error| AppError::internal(anyhow::anyhow!("Video submission: {error}")))?;

        let payload = self.read_json(response, "Video submission").await?;
        let task_id = payload
            .pointer("/output/task_id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                AppError::internal(anyhow::anyhow!("Bailian did not return a video task id."))
            })?;
        Ok(task_id.to_string())
    }

    /// One poll of `/api/v1/tasks/{id}`. Callers own the retry/backoff policy.
    pub async fn query_video_task(
        &self,
        api_key: &str,
        provider_task_id: &str,
    ) -> Result<VideoTaskSnapshot, AppError> {
        let url = format!(
            "{}/api/v1/tasks/{}",
            self.config.bailian.base_url,
            urlencode(provider_task_id)
        );
        let response = self
            .http
            .get(&url)
            .header(reqwest::header::AUTHORIZATION, Self::bearer(api_key))
            .send()
            .await
            .map_err(|error| AppError::internal(anyhow::anyhow!("Video task query: {error}")))?;

        let payload = self.read_json(response, "Video task query").await?;
        let output = payload
            .get("output")
            .cloned()
            .unwrap_or_else(|| Value::Object(Default::default()));
        let text = |key: &str| -> Option<String> {
            output
                .get(key)
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(|value| value.to_string())
        };

        let video_url = text("video_url");
        let raw_status = text("task_status").unwrap_or_else(|| "UNKNOWN".to_string());
        let message = text("message").or_else(|| text("code"));

        Ok(VideoTaskSnapshot {
            state: normalize_task_state(&raw_status, video_url.is_some()),
            raw_status,
            video_url,
            message,
        })
    }

    /// Downloads a generated asset into the storage directory.
    pub async fn store(&self, kind: TaskKind, source_url: &str) -> Result<StoredAsset, AppError> {
        let response = self
            .http
            .get(source_url)
            .send()
            .await
            .map_err(|error| {
                AppError::internal(anyhow::anyhow!(
                    "Failed to download generated {}: {error}",
                    kind.as_str()
                ))
            })?;
        let status = response.status();
        if !status.is_success() {
            return Err(AppError::internal(anyhow::anyhow!(
                "Failed to download generated {} (HTTP {}).",
                kind.as_str(),
                status.as_u16()
            )));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|error| AppError::internal(anyhow::anyhow!("reading generated bytes: {error}")))?;

        let directory = crate::media::asset_path::absolute(&self.config.storage_dir);
        tokio::fs::create_dir_all(&directory).await.map_err(|error| {
            AppError::internal(anyhow::anyhow!(
                "creating {}: {error}",
                directory.display()
            ))
        })?;

        let extension = resolve_extension(kind, source_url);
        let file_path = directory.join(format!(
            "{}-{}-{}{extension}",
            kind.as_str(),
            stamp(),
            suffix()
        ));
        tokio::fs::write(&file_path, &bytes).await.map_err(|error| {
            AppError::internal(anyhow::anyhow!(
                "writing {}: {error}",
                file_path.display()
            ))
        })?;

        Ok(StoredAsset {
            kind,
            source_url: Some(source_url.to_string()),
            mime: mime_for(extension),
            bytes: bytes.len() as i64,
            file_path: file_path.to_string_lossy().into_owned(),
        })
    }

    async fn post_json(
        &self,
        url: &str,
        api_key: &str,
        body: &Value,
        label: &str,
    ) -> Result<Value, AppError> {
        let response = self
            .http
            .post(url)
            .header(reqwest::header::AUTHORIZATION, Self::bearer(api_key))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(body)
            .send()
            .await
            .map_err(|error| AppError::internal(anyhow::anyhow!("{label}: {error}")))?;
        self.read_json(response, label).await
    }

    /// Provider failure text is capped: a proxy's HTML page would otherwise land
    /// in the model's tool result and in the conversation.
    async fn read_json(&self, response: reqwest::Response, label: &str) -> Result<Value, AppError> {
        let status = response.status();
        let raw = response
            .text()
            .await
            .map_err(|error| AppError::internal(anyhow::anyhow!("{label}: {error}")))?;
        if !status.is_success() {
            let truncated: String = raw.chars().take(300).collect();
            return Err(AppError::internal(anyhow::anyhow!(
                "{label} failed (HTTP {}): {truncated}",
                status.as_u16()
            )));
        }
        serde_json::from_str::<Value>(&raw).map_err(|_| {
            let truncated: String = raw.chars().take(200).collect();
            AppError::internal(anyhow::anyhow!(
                "Bailian returned non-JSON {label} response: {truncated}"
            ))
        })
    }
}

/// `image-2026-09-22T05-40-07-138Z-6ffnn9.png` — the shape already in `storage/`.
fn stamp() -> String {
    chrono::Utc::now()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        .replace(':', "-")
        .replace('.', "-")
}

fn suffix() -> String {
    const ALPHABET: [u8; 36] = *b"0123456789abcdefghijklmnopqrstuvwxyz";
    crate::config::random_bytes(6)
        .into_iter()
        .map(|byte| ALPHABET[usize::from(byte) % ALPHABET.len()] as char)
        .collect()
}

fn resolve_extension(kind: TaskKind, source_url: &str) -> &'static str {
    let from_url = url::Url::parse(source_url)
        .ok()
        .and_then(|parsed| {
            parsed
                .path_segments()
                .and_then(|mut segments| segments.next_back())
                .map(|last| last.to_ascii_lowercase())
        })
        .and_then(|name| {
            name
                .rsplit_once('.')
                .map(|(_, extension)| format!(".{extension}"))
        })
        .unwrap_or_default();
    allowed_extensions(kind)
        .iter()
        .find(|allowed| **allowed == from_url)
        .copied()
        .unwrap_or_else(|| default_extension(kind))
}

fn mime_for(extension: &str) -> String {
    MIME_BY_EXTENSION
        .iter()
        .find(|(needle, _)| *needle == extension)
        .map(|(_, mime)| mime.to_string())
        .unwrap_or_else(|| "application/octet-stream".to_string())
}

/// Task ids are opaque text that goes straight into the request path.
fn urlencode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_two_shipped_image_models_are_accepted() {
        assert!(assert_image_model("wan2.7-image").is_ok());
        assert!(assert_image_model("wan2.7-image-pro").is_ok());
        let error = assert_image_model("nope").expect_err("rejected");
        assert!(error.contains("Unsupported image model \"nope\""), "{error}");
        assert!(error.contains("wan2.7-image-pro"), "the message lists what works");
    }

    #[test]
    fn an_unknown_provider_status_is_a_failure_and_success_without_a_url_is_too() {
        use VideoTaskState::*;
        assert_eq!(normalize_task_state("PENDING", false), Pending);
        assert_eq!(normalize_task_state("RUNNING", false), Running);
        assert_eq!(normalize_task_state("SUCCEEDED", true), Succeeded);
        // Pinning the rule that keeps the worker from polling forever.
        assert_eq!(normalize_task_state("SUCCEEDED", false), Failed);
        for raw in ["", "UNKNOWN", "CANCELED", "succeeded"] {
            assert_eq!(normalize_task_state(raw, true), Failed, "{raw}");
        }
    }

    #[test]
    fn extensions_are_taken_from_the_url_only_when_the_kind_allows_them() {
        let url = |path: &str| format!("https://provider.test{path}");
        assert_eq!(resolve_extension(TaskKind::Image, &url("/a/b.png")), ".png");
        assert_eq!(
            resolve_extension(TaskKind::Image, &url("/a/b.MP4")),
            ".png",
            "a video is not an allowed image extension, so the default wins"
        );
        assert_eq!(resolve_extension(TaskKind::Video, &url("/a/b.webm")), ".webm");
        assert_eq!(resolve_extension(TaskKind::Video, &url("/a/b.gif")), ".mp4");
        assert_eq!(
            resolve_extension(TaskKind::Image, &url("/a/b")),
            ".png",
            "no extension at all"
        );
        assert_eq!(
            resolve_extension(TaskKind::Image, "not a url"),
            ".png",
            "an unparsable URL must not abort the download"
        );
        assert_eq!(
            resolve_extension(TaskKind::Image, &url("/a/b.png?Expires=7")),
            ".png",
            "the query string is not part of the path"
        );
    }

    #[test]
    fn mimes_match_the_extensions_the_storage_dir_uses() {
        assert_eq!(mime_for(".png"), "image/png");
        assert_eq!(mime_for(".jpeg"), "image/jpeg");
        assert_eq!(mime_for(".mp4"), "video/mp4");
        assert_eq!(mime_for(".mov"), "video/quicktime");
        assert_eq!(mime_for(".nope"), "application/octet-stream");
    }

    #[test]
    fn file_names_keep_the_shape_the_downloads_already_serve() {
        // image-2026-09-22T05-40-07-138Z-6ffnn9.png
        let stamp = stamp();
        assert_eq!(stamp.chars().nth(10), Some('T'), "date and time separator");
        assert!(!stamp.contains(':') && !stamp.contains('.'), "colons and dots are replaced");
        assert!(stamp.ends_with('Z'), "UTC");
        let first = suffix();
        let second = suffix();
        assert_eq!(first.chars().count(), 6);
        assert!(
            first.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
            "{first}"
        );
        assert_ne!(first, second, "the suffix is what keeps two renders apart");
    }

    #[test]
    fn task_ids_are_encoded_into_the_path() {
        assert_eq!(urlencode("abc-123_4.5~6"), "abc-123_4.5~6");
        assert_eq!(urlencode("a/b"), "a%2Fb");
        assert_eq!(urlencode("任务"), "%E4%BB%BB%E5%8A%A1", "UTF-8 percent encoding");
    }
}
