use std::time::Duration;

use serde_json::{json, Value};

use crate::bailian::responses::FunctionTool;
use crate::common::args::{clamp, read_number, read_optional_number, read_string};
use crate::common::signed_url::build_signed_frame_url;
use crate::media::service::DEFAULT_IMAGE_MODEL;
use crate::media::StoredAsset;
use crate::state::AppState;
use crate::task::service::NewTask;
use crate::task::TaskKind;

use super::mode::Mode;

/// A single image generation call leaves the stream silent for tens of seconds.
/// The timeout aborts the tool rather than the connection, so the failure comes
/// back to the model as a tool result it can react to.
const TOOL_TIMEOUT: Duration = Duration::from_secs(120);

const DEFAULT_VIDEO_SIZE: &str = "1280*720";
const DEFAULT_VIDEO_DURATION: i64 = 5;
const MIN_VIDEO_DURATION: i64 = 1;
const MAX_VIDEO_DURATION: i64 = 10;

/// Text handed back to the model as a tool result, with the ids of any assets it
/// just produced appended.
///
/// The ids are what let a later call name a specific image — image-to-video takes
/// one as its first frame. Shared by the text turn loop and the voice session.
pub fn describe_outcome(outcome: &ToolOutcome, asset_ids: &[i64]) -> String {
    if asset_ids.is_empty() {
        return outcome.output.clone();
    }
    format!(
        "{} Asset ids for later reference: {}.",
        outcome.output,
        asset_ids
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    )
}

#[derive(Debug, Clone)]
pub struct ToolOutcome {
    pub ok: bool,
    /// Text fed back to the model as the tool result.
    pub output: String,
    /// Assets produced by this call, to be persisted and pushed to the browser.
    pub assets: Vec<StoredAsset>,
}

impl ToolOutcome {
    fn failure(output: impl Into<String>) -> Self {
        Self { ok: false, output: output.into(), assets: vec![] }
    }

    fn success(output: impl Into<String>) -> Self {
        Self { ok: true, output: output.into(), assets: vec![] }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ToolContext {
    pub conversation_id: i64,
    /// Assistant placeholder of the current turn; async tasks link back to it.
    pub message_id: i64,
}

fn generate_image_spec() -> FunctionTool {
    FunctionTool::new(
        "generate_image",
        "Generate one or more images from a text prompt and save them locally. Use this whenever the user asks for a picture, illustration, poster, icon or any other bitmap asset.",
        json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "Text description of the desired image. Write it in the same language as the user request, and make it visually specific (subject, style, lighting, composition)."
                },
                "size": {
                    "type": "string",
                    "description": "Output size such as 1024*1024. Defaults to 1024*1024."
                },
                "count": {
                    "type": "integer",
                    "description": "How many images to generate, 1 to 4. Defaults to 1."
                }
            },
            "required": ["prompt"],
            "additionalProperties": false
        }),
    )
}

fn generate_video_spec() -> FunctionTool {
    FunctionTool::new(
        "generate_video",
        "Submit a text-to-video (or image-to-video) render job. Returns immediately with a task id: the render takes minutes and the finished video is pushed to the page automatically, so do not submit the same job twice and do not poll for it.",
        json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "What should happen in the video: subject, action, camera movement, lighting, mood. Write it in the same language as the user request."
                },
                "size": {
                    "type": "string",
                    "description": "Output size such as 1280*720. Defaults to 1280*720."
                },
                "duration": {
                    "type": "integer",
                    "description": format!("Clip length in seconds, {MIN_VIDEO_DURATION} to {MAX_VIDEO_DURATION}. Defaults to {DEFAULT_VIDEO_DURATION}.")
                },
                "first_frame_asset_id": {
                    "type": "integer",
                    "description": "Animate an existing image from this conversation by using it as the first frame. Pass the asset id that generate_image reported for that image. Omit this parameter for a normal text-to-video render."
                }
            },
            "required": ["prompt"],
            "additionalProperties": false
        }),
    )
}

fn get_video_task_spec() -> FunctionTool {
    FunctionTool::new(
        "get_video_task",
        "Check the status of a video task submitted earlier in this conversation. Only call this when the user asks about progress; finished videos arrive on their own.",
        json!({
            "type": "object",
            "properties": {
                "task_id": {
                    "type": "integer",
                    "description": "The task id returned by generate_video."
                }
            },
            "required": ["task_id"],
            "additionalProperties": false
        }),
    )
}

/// Names in registration order, which is also the order `all_specs` returns.
pub const TOOL_NAMES: [&str; 3] = ["generate_image", "generate_video", "get_video_task"];

/// The tool registry: name to behaviour, narrowed per mode.
///
/// Adding a tool means writing one function, adding its spec here, and listing
/// its name in the modes that may see it (`Mode::tool_names`).
pub struct ToolRegistry {
    state: AppState,
}

impl ToolRegistry {
    pub fn new(state: AppState) -> Self {
        Self { state }
    }

    fn spec(name: &str) -> Option<FunctionTool> {
        match name {
            "generate_image" => Some(generate_image_spec()),
            "generate_video" => Some(generate_video_spec()),
            "get_video_task" => Some(get_video_task_spec()),
            _ => None,
        }
    }

    pub fn specs_for(mode: Mode) -> Vec<FunctionTool> {
        mode.tool_names()
            .iter()
            .filter_map(|name| Self::spec(name))
            .collect()
    }

    /// Every registered tool, ungated by mode. For the voice session, which runs
    /// in none of the four modes (docs/architecture.md §11.3) — `specs_for(auto)`
    /// returns the same three today but would silently shrink if `auto` is ever
    /// narrowed.
    pub fn all_specs() -> Vec<FunctionTool> {
        TOOL_NAMES.iter().filter_map(|name| Self::spec(name)).collect()
    }

    /// Runs one tool by name under a hard timeout.
    ///
    /// Shared by the text turn loop and the voice session so the two cannot drift
    /// on the timeout or on the shape of a failure handed back to the model.
    pub async fn execute(&self, name: &str, args: &Value, context: ToolContext) -> ToolOutcome {
        let state = self.state.clone();
        let api_key = match state.require_key().await {
            Ok(key) => key,
            Err(error) => return ToolOutcome::failure(format!("Tool failed: {}", error.describe())),
        };
        let future = async move {
            match name {
                "generate_image" => generate_image(&state, &api_key, args, context).await,
                "generate_video" => generate_video(&state, &api_key, args, context).await,
                "get_video_task" => get_video_task(&state, args, context).await,
                _ => ToolOutcome::failure(format!("Unknown tool: {name}")),
            }
        };

        match tokio::time::timeout(TOOL_TIMEOUT, future).await {
            Ok(outcome) => outcome,
            // The Nest version aborted the in-flight fetch and its error surfaced
            // as `AbortError`, which `describeError` turned into this sentence.
            Err(_) => ToolOutcome::failure("Tool failed: 请求已取消。"),
        }
    }
}

async fn generate_image(
    state: &AppState,
    api_key: &str,
    args: &Value,
    _context: ToolContext,
) -> ToolOutcome {
    let prompt = read_string(args, "prompt", "");
    if prompt.is_empty() {
        return ToolOutcome::failure("prompt is required.");
    }
    let size = read_string(args, "size", "1024*1024");
    let count = clamp(read_number(args, "count", 1.0), 1, 4) as u32;

    let urls = match state
        .media
        .generate_images(api_key, &prompt, DEFAULT_IMAGE_MODEL, &size, count)
        .await
    {
        Ok(urls) => urls,
        Err(error) => return ToolOutcome::failure(format!("Tool failed: {}", error.describe())),
    };

    let mut assets: Vec<StoredAsset> = Vec::new();
    for url in urls {
        match state.media.store(TaskKind::Image, &url).await {
            Ok(stored) => assets.push(stored),
            Err(error) => return ToolOutcome::failure(format!("Tool failed: {}", error.describe())),
        }
    }

    ToolOutcome {
        ok: true,
        output: format!(
            "Generated {} image(s) at {size} with {DEFAULT_IMAGE_MODEL}. The images are already saved locally and rendered to the user; do not emit markdown image links or raw URLs, just describe them briefly.",
            assets.len()
        ),
        assets,
    }
}

async fn generate_video(
    state: &AppState,
    api_key: &str,
    args: &Value,
    context: ToolContext,
) -> ToolOutcome {
    let prompt = read_string(args, "prompt", "");
    if prompt.is_empty() {
        return ToolOutcome::failure("prompt is required.");
    }
    let size = read_string(args, "size", DEFAULT_VIDEO_SIZE);
    let duration = clamp(
        read_number(args, "duration", DEFAULT_VIDEO_DURATION as f64),
        MIN_VIDEO_DURATION,
        MAX_VIDEO_DURATION,
    );
    let requested_asset_id = read_optional_number(args, "first_frame_asset_id");

    // Scoped to the conversation, so an asset id cannot point at another user's
    // image.
    let first_frame = match requested_asset_id {
        None => None,
        Some(asset_id) => state
            .conversations
            .image_asset_in(context.conversation_id, asset_id)
            .await
            .ok()
            .flatten(),
    };

    if requested_asset_id.is_some() && first_frame.is_none() {
        return ToolOutcome::failure(format!(
            "first_frame_asset_id {} is not an image in this conversation.",
            requested_asset_id.expect("checked above")
        ));
    }
    if first_frame.is_some() && !state.config.is_publicly_reachable() {
        return ToolOutcome::failure(
            "Image-to-video needs a publicly reachable frame URL, but PUBLIC_BASE_URL points at localhost. Generate the video from the prompt alone, or configure a public base URL.",
        );
    }

    let bailian = &state.config.bailian;
    let model = if first_frame.is_none() { bailian.video_model_t2v.clone() } else { bailian.video_model_i2v.clone() };
    let frame_url = first_frame
        .as_ref()
        .map(|(id, _path)| build_signed_frame_url(&state.config, *id, chrono::Utc::now().timestamp_millis()));

    let provider_task_id = match state
        .media
        .submit_video(
            api_key,
            &prompt,
            &model,
            &size,
            duration as u32,
            frame_url.as_deref(),
        )
        .await
    {
        Ok(id) => id,
        Err(error) => return ToolOutcome::failure(format!("Tool failed: {}", error.describe())),
    };

    let task_id = match state
        .tasks
        .create(&NewTask {
            conversation_id: context.conversation_id,
            message_id: Some(context.message_id),
            kind: TaskKind::Video,
            provider_task_id,
            model: model.clone(),
            prompt: prompt.clone(),
            params: json!({
                "size": size,
                "duration": duration,
                "firstFrameAssetId": first_frame.as_ref().map(|(id, _)| *id),
            }),
        })
        .await
    {
        Ok(id) => id,
        Err(error) => return ToolOutcome::failure(format!("Tool failed: {}", error.describe())),
    };

    let frame_note = match first_frame.as_ref() {
        None => String::new(),
        Some((id, _)) => format!("，以资产 {id} 作为首帧"),
    };

    ToolOutcome::success(format!(
        "Video task {task_id} submitted with {model} at {size}, {duration}s{frame_note}. The render takes a few minutes and the result is pushed to the page automatically. Tell the user the task is under way; do not submit it again."
    ))
}

async fn get_video_task(state: &AppState, args: &Value, context: ToolContext) -> ToolOutcome {
    let Some(task_id) = read_optional_number(args, "task_id") else {
        return ToolOutcome::failure("task_id is required.");
    };

    let task = match state.tasks.find_by_id(task_id).await {
        Ok(Some(task)) => task,
        Ok(None) => {
            return ToolOutcome::failure(format!("No video task {task_id} in this conversation."))
        }
        Err(error) => return ToolOutcome::failure(format!("Tool failed: {}", error.describe())),
    };
    if task.conversation_id != context.conversation_id {
        return ToolOutcome::failure(format!("No video task {task_id} in this conversation."));
    }

    let detail = match task.error.as_deref() {
        None => String::new(),
        Some(error) => format!("，原因：{error}"),
    };
    ToolOutcome::success(format!(
        "Video task {task_id} is {} ({}){detail}.",
        task.status.label(),
        task.status.as_str()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
    use crate::task::TaskStatus;
    use serde_json::json;

    /// A state over a real in-memory database, so the guard rails are exercised
    /// against the same visibility rules the running server uses. `public_base_url`
    /// is localhost on purpose: that is the configuration the image-to-video guard
    /// exists for.
    async fn state() -> AppState {
        let pool = crate::db::open_in_memory().await;
        sqlx::query("INSERT INTO users (id, username, password_hash, role, created_at) VALUES (1,'u','h','user',1)")
            .execute(&pool)
            .await
            .expect("user");
        sqlx::query("INSERT INTO conversations (id, user_id, title, created_at, updated_at) VALUES (1,1,'t',1,1)")
            .execute(&pool)
            .await
            .expect("conversation");
        let config = AppConfig::test("/tmp/kestrel-tool-tests", "http://localhost:8848");
        AppState::new(config, pool, Some("test-key".to_string()))
    }

    fn args(value: Value) -> Value {
        value
    }

    const CONTEXT: ToolContext = ToolContext { conversation_id: 1, message_id: 2 };

    #[test]
    fn each_mode_advertises_its_own_slice_of_the_registry() {
        let names = |mode: Mode| -> Vec<String> {
            ToolRegistry::specs_for(mode).iter().map(|spec| spec.name.clone()).collect()
        };
        assert!(names(Mode::Chat).is_empty(), "a chat turn cannot spend money");
        assert_eq!(names(Mode::Image), vec!["generate_image"]);
        assert_eq!(
            names(Mode::Video),
            vec!["generate_video", "get_video_task"]
        );
        assert_eq!(
            names(Mode::Auto),
            vec!["generate_image", "generate_video", "get_video_task"]
        );
        assert_eq!(ToolRegistry::all_specs().len(), 3, "voice sees every tool");
    }

    #[test]
    fn specs_carry_the_schema_the_model_needs_to_call_them() {
        let specs = ToolRegistry::all_specs();
        let image = specs.iter().find(|spec| spec.name == "generate_image").expect("spec");
        assert_eq!(image.r#type, "function");
        assert!(image.description.contains("bitmap asset"));
        assert_eq!(image.parameters["required"], json!(["prompt"]));
        assert_eq!(image.parameters["additionalProperties"], json!(false));

        let video = specs.iter().find(|spec| spec.name == "generate_video").expect("spec");
        assert!(video.description.contains("do not poll"), "the text is what stops the model polling");
        assert!(video.parameters["properties"]["duration"]["description"]
            .as_str()
            .expect("description")
            .contains("1 to 10"));
        assert!(video.parameters["properties"].get("first_frame_asset_id").is_some());

        let task = specs.iter().find(|spec| spec.name == "get_video_task").expect("spec");
        assert_eq!(task.parameters["required"], json!(["task_id"]));
    }

    #[test]
    fn asset_ids_are_appended_for_the_next_call_to_name() {
        let outcome = ToolOutcome::success("done");
        assert_eq!(describe_outcome(&outcome, &[]), "done");
        assert_eq!(
            describe_outcome(&outcome, &[7, 8]),
            "done Asset ids for later reference: 7, 8."
        );
    }

    #[tokio::test]
    async fn an_unreachable_base_url_stops_image_to_video_before_the_submit() {
        let state = state().await;
        // An image asset that really belongs to this conversation.
        let message = state
            .conversations
            .append_assistant_message(1, "图好了", Some(Mode::Image))
            .await
            .expect("message");
        let asset_id = state
            .conversations
            .add_asset(
                message,
                &StoredAsset {
                    kind: TaskKind::Image,
                    file_path: "/tmp/kestrel-tool-tests/frame.png".to_string(),
                    source_url: None,
                    mime: "image/png".to_string(),
                    bytes: 4,
                },
            )
            .await
            .expect("asset");

        let outcome = generate_video(
            &state,
            "test-key",
            &args(json!({ "prompt": "让它动起来", "first_frame_asset_id": asset_id })),
            CONTEXT,
        )
        .await;
        assert!(!outcome.ok);
        assert!(outcome.output.contains("PUBLIC_BASE_URL points at localhost"), "{}", outcome.output);
        assert!(outcome.assets.is_empty());
    }

    #[tokio::test]
    async fn a_frame_id_from_another_conversation_is_refused() {
        let state = state().await;
        let outcome = generate_video(
            &state,
            "test-key",
            &args(json!({ "prompt": "动起来", "first_frame_asset_id": 999 })),
            CONTEXT,
        )
        .await;
        assert!(!outcome.ok);
        assert_eq!(
            outcome.output,
            "first_frame_asset_id 999 is not an image in this conversation."
        );
    }

    #[tokio::test]
    async fn a_missing_prompt_fails_without_calling_the_provider() {
        let state = state().await;
        for tool in [
            generate_image(&state, "k", &args(json!({})), CONTEXT).await,
            generate_video(&state, "k", &args(json!({ "prompt": "  " })), CONTEXT).await,
        ] {
            assert!(!tool.ok, "{}", tool.output);
            assert_eq!(tool.output, "prompt is required.");
        }
    }

    #[tokio::test]
    async fn the_task_tool_only_answers_for_its_own_conversation() {
        let state = state().await;
        let missing = get_video_task(&state, &args(json!({})), CONTEXT).await;
        assert!(!missing.ok);
        assert_eq!(missing.output, "task_id is required.");

        let foreign = get_video_task(&state, &args(json!({ "task_id": 5 })), CONTEXT).await;
        assert!(!foreign.ok);
        assert_eq!(foreign.output, "No video task 5 in this conversation.");

        let task_id = state
            .tasks
            .create(&NewTask {
                conversation_id: 1,
                message_id: Some(2),
                kind: TaskKind::Video,
                provider_task_id: "p-1".to_string(),
                model: "t2v-model".to_string(),
                prompt: "红隼俯冲".to_string(),
                params: json!({ "size": "1280*720" }),
            })
            .await
            .expect("task");
        let found = get_video_task(&state, &args(json!({ "task_id": task_id })), CONTEXT).await;
        assert!(found.ok, "{}", found.output);
        assert_eq!(
            found.output,
            format!("Video task {task_id} is 排队中 (queued).")
        );

        state
            .tasks
            .finish(task_id, TaskStatus::Failed, None, None, Some("供应商返回状态 FAILED"))
            .await
            .expect("finish");
        let failed = get_video_task(&state, &args(json!({ "task_id": task_id })), CONTEXT).await;
        assert!(failed.output.contains("失败 (failed)"), "{}", failed.output);
        assert!(failed.output.contains("原因：供应商返回状态 FAILED"), "{}", failed.output);

        // Another conversation's task is not visible here, and vice versa.
        sqlx::query("INSERT INTO conversations (id, user_id, title, created_at, updated_at) VALUES (2,1,'other',1,1)")
            .execute(&state.pool)
            .await
            .expect("conversation");
        let elsewhere = get_video_task(
            &state,
            &args(json!({ "task_id": task_id })),
            ToolContext { conversation_id: 2, message_id: 3 },
        )
        .await;
        assert!(!elsewhere.ok);
    }
}
