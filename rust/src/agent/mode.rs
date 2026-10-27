use serde::{Deserialize, Serialize};

/// A turn's capability mode. The selector is per turn, not per conversation:
/// one conversation can chat, then draw, then animate, and each turn is stored
/// with the mode it ran in so history renders correctly.
///
/// `auto` exposes every tool and lets the model route itself; the explicit modes
/// narrow the tool set so a turn cannot silently spend money on a generation the
/// user did not ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Auto,
    Chat,
    Image,
    Video,
}

pub const DEFAULT_MODE: Mode = Mode::Auto;
pub const MODES: [Mode; 4] = [Mode::Auto, Mode::Chat, Mode::Image, Mode::Video];

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Chat => "chat",
            Self::Image => "image",
            Self::Video => "video",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "智能",
            Self::Chat => "对话",
            Self::Image => "图片",
            Self::Video => "视频",
        }
    }

    /// Tool names advertised to the model per mode. `chat` deliberately has none.
    pub fn tool_names(self) -> &'static [&'static str] {
        match self {
            Self::Auto => &["generate_image", "generate_video", "get_video_task"],
            Self::Chat => &[],
            Self::Image => &["generate_image"],
            Self::Video => &["generate_video", "get_video_task"],
        }
    }

    /// `mode` is nullable because a voice turn runs in none of the four modes —
    /// voice is a separate surface, not a fifth mode (docs/architecture.md §11).
    pub fn parse(value: Option<&str>) -> Mode {
        let Some(raw) = value else {
            return DEFAULT_MODE;
        };
        let normalized = raw.trim().to_lowercase();
        MODES
            .into_iter()
            .find(|mode| mode.as_str() == normalized)
            .unwrap_or(DEFAULT_MODE)
    }

    /// Reading the `mode` column back. Unlike `parse` there is no default: null
    /// means "no mode" (a voice turn), and a value this build does not know must
    /// not be relabelled as one it does.
    pub fn from_storage(raw: Option<&str>) -> Option<Mode> {
        let raw = raw?;
        MODES.into_iter().find(|mode| mode.as_str() == raw)
    }

    pub fn build_instructions(self) -> String {
        const BASE: [&str; 7] = [
            "你是一个生成式媒体助手，运行在一个网页应用里。",
            "",
            "规则：",
            "- 用中文回复用户。",
            "- 生成前先用一两句话说明你的构思（这一步会被实时展示给用户）。",
            "- 生成的图片或视频会自动渲染到页面上，不要输出 markdown 图片语法，也不要贴 URL。",
            "- 生成后用一两句话简要说明你产出了什么。",
        ];
        let suffix: &[&str] = match self {
            Self::Auto => &[
                "当前是智能模式：根据用户意图自行判断是闲聊、生成图片还是生成视频。",
                "如果用户只是在闲聊或提问，不涉及生成，就直接回答，不要调用工具。",
            ],
            Self::Chat => &[
                "当前是纯对话模式：本次对话不提供任何生成工具。",
                "如果用户要求生成图片或视频，告诉他切换到对应的模式后再试，不要假装已经生成。",
            ],
            Self::Image => &["当前是图片模式：用户想要图片时，调用 generate_image。"],
            Self::Video => &[
                "当前是视频模式：用户想要视频时，调用 generate_video。",
                "文生视频直接传 prompt；图生视频需要额外传 first_frame_asset_id，",
                "该 id 是之前调用 generate_image 时工具结果里给出的 asset id。",
                "视频生成是异步任务，提交后会立刻返回，完成后服务端会自动把结果推送到页面。",
                "因此提交后不要反复调用 get_video_task 轮询，直接告诉用户任务已提交即可。",
            ],
        };
        let mut lines: Vec<&str> = BASE.to_vec();
        lines.push("");
        lines.extend_from_slice(suffix);
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_modes_degrade_to_auto() {
        assert_eq!(Mode::parse(None), Mode::Auto);
        assert_eq!(Mode::parse(Some(" Image ")), Mode::Image);
        assert_eq!(Mode::parse(Some("video")), Mode::Video);
        assert_eq!(Mode::parse(Some("nonsense")), Mode::Auto);
    }

    #[test]
    fn chat_advertises_no_tools_and_auto_advertises_all_three() {
        assert!(Mode::Chat.tool_names().is_empty());
        assert_eq!(Mode::Auto.tool_names().len(), 3);
        assert_eq!(Mode::Image.tool_names(), ["generate_image"]);
        assert_eq!(Mode::Video.tool_names(), ["generate_video", "get_video_task"]);
    }

    #[test]
    fn instructions_carry_the_base_rules_plus_the_mode_fragment() {
        let auto = Mode::Auto.build_instructions();
        assert!(auto.starts_with("你是一个生成式媒体助手"));
        assert!(auto.contains("当前是智能模式"));
        assert!(!auto.contains("当前是视频模式"));
        assert!(Mode::Video.build_instructions().contains("提交后不要反复调用 get_video_task"));
    }

    #[test]
    fn a_mode_round_trips_through_json_as_its_wire_name() {
        assert_eq!(serde_json::to_string(&Mode::Image).expect("encode"), "\"image\"");
        assert_eq!(
            serde_json::from_str::<Mode>("\"chat\"").expect("decode"),
            Mode::Chat
        );
    }
}
