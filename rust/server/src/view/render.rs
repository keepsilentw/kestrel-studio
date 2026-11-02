use super::html::markdown;
use crate::agent::mode::Mode;
use crate::conversation::MessageView;

/// Template preparation shared by the two surfaces that render stored turns: the
/// chat page and the super admin's read-only view of someone else's conversation.

pub struct RenderedMessage {
    pub view: MessageView,
    pub content_html: String,
    pub mode_label: Option<&'static str>,
}

/// One entry of the right-hand turn index. A turn is opened by a user message, so
/// that message is both the label and the scroll target.
pub struct TurnAnchor {
    pub message_id: i64,
    pub label: String,
}

/// Assistant text is markdown and the template injects the result unescaped.
///
/// The user's own words are NOT markdown, and must not be parsed here either: the
/// client prints them with `textContent` (`buildUserTurn`), so parsing on the
/// server made a reloaded turn differ from the live one. An empty `content_html`
/// is what tells the template to print the text escaped instead.
pub fn render_messages(views: Vec<MessageView>) -> Vec<RenderedMessage> {
    views
        .into_iter()
        .map(|view| {
            let content_html = if view.role == "assistant" && !view.content.is_empty() {
                markdown(&view.content)
            } else {
                String::new()
            };
            RenderedMessage {
                mode_label: view.mode.map(Mode::label),
                content_html,
                view,
            }
        })
        .collect()
}

pub fn build_turn_anchors(views: &[RenderedMessage]) -> Vec<TurnAnchor> {
    views
        .iter()
        .filter(|view| view.view.role == "user" && !view.view.content.trim().is_empty())
        .map(|view| TurnAnchor {
            message_id: view.view.id,
            label: view.view.content.trim().to_string(),
        })
        .collect()
}

/// Timestamps for the admin tables.
///
/// Pinned to the deployment's timezone so a rendered time does not depend on the
/// process's `TZ` — which is what `toLocaleString('zh-CN', { timeZone:
/// 'Asia/Shanghai' })` guaranteed. There is no tz database in this build, so the
/// offset is stated instead: China has had no DST since 1991, and the deployment
/// is one region.
pub fn format_timestamp(ms: i64) -> String {
    let shanghai = chrono::FixedOffset::east_opt(8 * 60 * 60).expect("a +08:00 offset");
    match chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms) {
        Some(moment) => moment
            .with_timezone(&shanghai)
            .format("%Y/%m/%d %H:%M")
            .to_string(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::TaskKind;

    fn message(id: i64, role: &str, content: &str, mode: Option<Mode>) -> MessageView {
        MessageView {
            id,
            role: role.to_string(),
            content: content.to_string(),
            reasoning: None,
            tool_calls: None,
            mode,
            created_at: 1_700_000_000_000,
            assets: vec![],
        }
    }

    #[test]
    fn only_assistant_text_is_parsed_as_markdown() {
        let rendered = render_messages(vec![
            message(1, "user", "**原始输入**", None),
            message(2, "assistant", "**好的**", Some(Mode::Image)),
            message(3, "assistant", "", Some(Mode::Chat)),
        ]);
        assert_eq!(rendered[0].content_html, "", "the user's words stay plain text");
        assert_eq!(rendered[1].content_html, "<p><strong>好的</strong></p>");
        assert_eq!(rendered[1].mode_label, Some("图片"));
        assert_eq!(rendered[2].content_html, "", "an empty body renders nothing");
        assert_eq!(rendered[0].mode_label, None, "a voice turn has no mode");
    }

    #[test]
    fn the_turn_index_is_opened_by_each_user_message() {
        let rendered = render_messages(vec![
            message(1, "user", "  画一只红隼  ", None),
            message(2, "assistant", "好了", None),
            message(3, "user", "   ", None),
            message(4, "user", "再来一张", None),
        ]);
        let anchors = build_turn_anchors(&rendered);
        assert_eq!(
            anchors.iter().map(|a| (a.message_id, a.label.as_str())).collect::<Vec<_>>(),
            vec![(1, "画一只红隼"), (4, "再来一张")],
            "trimmed, and a blank prompt is not a turn"
        );
    }

    #[test]
    fn timestamps_do_not_follow_the_process_timezone() {
        // 1.7e12 ms is 2023-11-14 22:13:20 UTC, i.e. 06:13 the next day in +08:00:
        // the day rolls over, which is exactly what pinning the offset is for.
        // Cross-checked with the system: TZ=Asia/Shanghai date -r 1700000000.
        // The separator is a slash because that is what `toLocaleString('zh-CN')`
        // produced, and the admin tables are the only consumer.
        assert_eq!(format_timestamp(1_700_000_000_000), "2023/11/15 06:13");
        assert_eq!(
            format_timestamp(i64::MAX),
            "",
            "a value outside the representable range prints nothing, not a wrong time"
        );
    }

    #[test]
    fn assets_carry_their_kind_into_the_template() {
        let mut view = message(1, "assistant", "好了", None);
        view.assets = vec![crate::conversation::AssetView {
            id: 9,
            kind: TaskKind::Video,
            url: "/api/v1/assets/9/download".to_string(),
            mime: "video/mp4".to_string(),
            bytes: 4,
        }];
        let rendered = render_messages(vec![view]);
        assert_eq!(rendered[0].view.assets[0].kind, TaskKind::Video);
    }
}
