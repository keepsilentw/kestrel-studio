//! The mode's agent-side behaviour. The type itself is in `kestrel_proto::event`.
//!
//! Split that way because the browser needs the mode's name and label — nothing
//! else. Which tools a mode advertises, and the system prompt it builds, are this
//! server's business; putting them in the shared crate would ship the prompt to
//! the client as dead code and invite the page to depend on it.
//!
//! Rust does not allow inherent methods on a foreign type, so these two live
//! behind a trait. Call sites read `mode.tool_names()` / `mode.build_instructions()`
//! exactly as before, with `ModePrompt` in scope.

pub use kestrel_proto::event::Mode;

// `DEFAULT_MODE` and `MODES` are deliberately **not** re-exported: nothing in this
// tree reads them (they only matter to `Mode::parse`, which lives with the type),
// and a re-export nobody uses is a second place a name can drift.

pub trait ModePrompt {
    /// Tool names advertised to the model per mode. `chat` deliberately has none.
    fn tool_names(self) -> &'static [&'static str];

    /// The instructions for a turn in this mode: the base rules plus the
    /// fragment that tells the model what it may and may not offer.
    fn build_instructions(self) -> String;
}

impl ModePrompt for Mode {
    fn tool_names(self) -> &'static [&'static str] {
        match self {
            Self::Auto => &["generate_image", "generate_video", "get_video_task"],
            Self::Chat => &[],
            Self::Image => &["generate_image"],
            Self::Video => &["generate_video", "get_video_task"],
        }
    }

    fn build_instructions(self) -> String {
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
