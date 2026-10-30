//! The template / client-script contract, and the first-paint behaviour.
//!
//! Two independent halves of the guarantee the Nest suite encoded:
//!
//! 1. the ids the page must carry are **read out of the client source**, not
//!    restated here, so editing `web/scripts/main.ts` to require a new element
//!    fails this test instead of failing in the browser console;
//! 2. the page is rendered from real rows, so escaping, the placeholder filter and
//!    the role-dependent chrome are checked as the browser will receive them.

use super::render::{TurnAnchor, build_turn_anchors, render_messages};
use crate::agent::mode::Mode;
use crate::conversation::MessageView;
use crate::task::TaskKind;

use super::html::{escape, markdown};
use super::templates::{ChatPage, ConversationEntry, PendingTask};

/// Every selector the client fetches with `requireElement`, parsed from the
/// source it ships in. A missing element is a hard failure there: the script
/// throws before it wires anything up.
fn required_ids() -> Vec<String> {
    let path = std::path::Path::new("../web/scripts/main.ts");
    let source = std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("{} must be readable for the contract test: {error}", path.display()));

    let mut ids = Vec::new();
    for line in source.lines().filter(|line| line.contains("requireElement")) {
        // requireElement<HTMLElement>('#stream')
        let Some(hash) = line.find("#") else { continue };
        let after = &line[hash + 1..];
        let end = after
            .find(|c: char| !c.is_ascii_alphanumeric() && c != '-')
            .unwrap_or(after.len());
        let id = &after[..end];
        if !id.is_empty() && !ids.iter().any(|existing| existing == id) {
            ids.push(id.to_string());
        }
    }
    assert!(ids.len() >= 15, "only found {ids:?} — the parser is out of date");
    ids
}

fn page_fixture(
    messages: &[crate::conversation::MessageView],
    conversations: &[ConversationEntry],
    active: Option<i64>,
    start_seq: Option<i64>,
    is_super: bool,
) -> ChatPage {
    let rendered = render_messages(messages.to_vec());
    let anchors = build_turn_anchors(&rendered);
    let anchors = if anchors.is_empty() {
        vec![TurnAnchor { message_id: 1, label: "画一只红隼".to_string() }]
    } else {
        anchors
    };
    ChatPage {
        username: if is_super { "boss" } else { "walker" }.to_string(),
        is_super,
        conversations: conversations.to_vec(),
        active_conversation_id: active,
        start_seq,
        messages: rendered,
        pending_tasks: vec![],
        turn_anchors: anchors,
        can_delete_conversations: !is_super,
    }
}

fn stored_message() -> MessageView {
    MessageView {
        id: 2,
        role: "assistant".to_string(),
        content: "好的，**已生成**。".to_string(),
        reasoning: Some("先想一下构图".to_string()),
        tool_calls: Some(vec![crate::conversation::ToolCallRecord {
            name: "generate_image".to_string(),
            arguments: serde_json::json!({ "prompt": "一只红隼" }),
            ok: true,
            summary: "Generated 1 image(s)".to_string(),
        }]),
        mode: Some(Mode::Image),
        created_at: 1,
        assets: vec![
            crate::conversation::AssetView {
                id: 7,
                kind: TaskKind::Image,
                url: "/api/v1/assets/7/download".to_string(),
                mime: "image/png".to_string(),
                bytes: 4,
            },
            crate::conversation::AssetView {
                id: 8,
                kind: TaskKind::Video,
                url: "/api/v1/assets/8/download".to_string(),
                mime: "video/mp4".to_string(),
                bytes: 8,
            },
        ],
    }
}

#[test]
fn the_chat_page_carries_every_element_the_client_script_requires() {
    let page = ChatPage {
        username: "walker".to_string(),
        is_super: false,
        conversations: vec![ConversationEntry { id: 1, title: "一只红隼".to_string(), is_active: true }],
        active_conversation_id: Some(1),
        start_seq: Some(3),
        messages: render_messages(vec![stored_message()]),
        pending_tasks: vec![PendingTask { id: 9, kind: TaskKind::Video, status_label: "渲染中".to_string() }],
        turn_anchors: vec![TurnAnchor { message_id: 1, label: "画一只红隼".to_string() }],
        can_delete_conversations: true,
    };
    let rendered = super::templates::chat_page(&page);

    for id in required_ids() {
        assert!(
            rendered.contains(&format!("id=\"{id}\"")),
            "the client requires #{id}, the template does not render it"
        );
    }
}

#[test]
fn the_body_cursor_attributes_match_what_the_stream_endpoint_expects() {
    let with_conversation = page_fixture(&[], &[], Some(4), Some(12), false);
    let rendered = super::templates::chat_page(&with_conversation);
    assert!(rendered.contains("data-conversation-id=\"4\""), "{rendered}");
    assert!(rendered.contains("data-start-seq=\"12\""));
    assert!(rendered.contains("data-can-delete-conversations=\"true\""));

    // Nothing painted: empty attributes, which is what makes the client open the
    // stream without `?from=` and let the server choose the catch-up point.
    let empty = page_fixture(&[], &[], None, None, false);
    let rendered = super::templates::chat_page(&empty);
    assert!(rendered.contains("data-conversation-id=\"\""), "{rendered}");
    assert!(rendered.contains("data-start-seq=\"\""));
    assert!(rendered.contains("class=\"conversation-empty\">暂无会话</li>"));
}

#[test]
fn role_changes_the_chrome_and_the_delete_affordance() {
    let conversations = [ConversationEntry {
        id: 1,
        title: "会话".to_string(),
        is_active: true,
    }];

    let normal =
        super::templates::chat_page(&page_fixture(&[], &conversations, Some(1), Some(0), false));
    assert!(!normal.contains("href=\"/admin\""), "no admin entry for a normal account");
    assert!(normal.contains("data-can-delete-conversations=\"true\""));
    assert!(normal.contains("data-delete-conversation=\"1\""), "{normal}");
    assert!(normal.contains("<span class=\"account-name\">walker</span>"));

    let admin =
        super::templates::chat_page(&page_fixture(&[], &conversations, Some(1), Some(0), true));
    assert!(admin.contains("href=\"/admin\""));
    assert!(admin.contains("data-can-delete-conversations=\"false\""));
    assert!(
        !admin.contains("data-delete-conversation="),
        "a super admin sees everyone's history and is not offered the hide"
    );
    assert!(admin.contains("<span class=\"account-name\">boss</span>"));
}

#[test]
fn user_text_is_escaped_and_assistant_html_is_not_double_escaped() {
    let mut prompt = stored_message();
    prompt.id = 1;
    prompt.role = "user".to_string();
    prompt.content = "<script>alert(1)</script> & \"引号\"".to_string();
    prompt.assets.clear();
    prompt.reasoning = None;
    prompt.tool_calls = None;

    let rendered = super::templates::chat_page(&page_fixture(
        &[prompt],
        &[ConversationEntry {
            id: 1,
            title: "A & B <b>".to_string(),
            is_active: true,
        }],
        Some(1),
        None,
        false,
    ));
    assert!(rendered.contains("&lt;script&gt;alert(1)&lt;/script&gt;"), "{rendered}");
    assert!(!rendered.contains("<script>alert"), "no executable script from user text");
    assert!(rendered.contains("A &amp; B &lt;b&gt;"), "the sidebar title is escaped too");
}

#[test]
fn a_turn_renders_the_trail_the_live_stream_built() {
    let rendered = super::templates::turn(&render_messages(vec![stored_message()]).remove(0));
    assert!(rendered.contains("class=\"turn turn-assistant\" data-message-id=\"2\""));
    assert!(rendered.contains("<span class=\"turn-mode\">图片</span>"));
    assert!(rendered.contains("<details class=\"reasoning\">"));
    assert!(rendered.contains("先想一下构图"));
    assert!(rendered.contains("<li class=\"tool-call\">generate_image — Generated 1 image(s)</li>"));
    assert!(rendered.contains("<div class=\"content\"><p>好的，<strong>已生成</strong>。</p></div>"));
    assert!(rendered.contains("<img src=\"/api/v1/assets/7/download\" alt=\"生成的图片\" loading=\"lazy\" class=\"asset-image\">"));
    assert!(rendered.contains("<video src=\"/api/v1/assets/8/download\" controls preload=\"metadata\" class=\"asset-video\"></video>"));
    assert!(rendered.contains("href=\"/api/v1/assets/8/download\" download class=\"asset-download\">"));
}

#[test]
fn escaping_covers_the_characters_templates_interpolate() {
    assert_eq!(
        escape("<a href='x'>&\""),
        "&lt;a href=&#39;x&#39;&gt;&amp;&quot;"
    );
    assert_eq!(escape("普通文本"), "普通文本");
}

#[test]
fn the_markdown_subset_matches_what_the_assistant_actually_writes() {
    assert_eq!(markdown(""), "");
    assert_eq!(markdown("只有一行"), "<p>只有一行</p>");
    assert_eq!(
        markdown("软\n换行"),
        "<p>软\n换行</p>",
        "a soft break stays inside the paragraph, as marked does"
    );
    assert_eq!(markdown("## 标题"), "<h2>标题</h2>");
    assert_eq!(markdown("`code` 与 **粗** 和 *斜*"), "<p><code>code</code> 与 <strong>粗</strong> 和 <em>斜</em></p>");
    assert_eq!(
        markdown("- 甲\n- 乙"),
        "<ul>\n<li>甲</li>\n<li>乙</li>\n</ul>"
    );
    assert_eq!(markdown("1. 甲\n2. 乙"), "<ol>\n<li>甲</li>\n<li>乙</li>\n</ol>");
    assert_eq!(
        markdown("```rust\nlet x = 1;\n```"),
        "<pre><code class=\"language-rust\">let x = 1;</code></pre>"
    );
    assert_eq!(
        markdown("```\n<b>not html</b>\n```"),
        "<pre><code>&lt;b&gt;not html&lt;/b&gt;</code></pre>",
        "fenced content is escaped, never forwarded"
    );
    assert_eq!(
        markdown("[红隼](https://example.test?a=1&b=2)"),
        "<p><a href=\"https://example.test?a=1&amp;b=2\">红隼</a></p>",
        "the URL is escaped exactly once"
    );
    assert_eq!(
        markdown("<script>alert(1)</script>"),
        "<p>&lt;script&gt;alert(1)&lt;/script&gt;</p>",
        "raw HTML from the model is not passed through, unlike marked"
    );
    assert_eq!(
        markdown("3 * 4 与 5 * 6"),
        "<p>3 <em> 4 与 5 </em> 6</p>",
        "known divergence from marked: stray asterisks pair up. Assistant prose rarely has them."
    );
    assert_eq!(
        markdown("`a **b** c`"),
        "<p><code>a **b** c</code></p>",
        "code spans are protected from the emphasis pass"
    );
}

/// The ids the WASM voice page looks up, read out of **its own source** — the
/// same device [`required_ids`] uses for the chat page and `web/scripts/main.ts`.
/// A shell that stops carrying one is a hard failure in the browser, so it has to
/// be a failing test first.
fn voice_required_ids() -> Vec<String> {
    let path = std::path::Path::new("../client/src/voice_page.rs");
    let source = std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("{} must be readable: {error}", path.display()));

    let mut ids = Vec::new();
    // Split on the call form, then read the literal that follows it. The piece
    // before the first hit is the file's header, which has no id in it.
    let mut pieces = source.split("element(&document, \"");
    pieces.next();
    for after in pieces {
        let Some(end) = after.find('"') else { continue };
        let id = &after[..end];
        // Only accept something that can actually be an element id. Without this,
        // a sentence in a comment mentioning the call form is picked up as a
        // fourth "element" and the test fails for the wrong reason.
        let shaped = !id.is_empty()
            && id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if shaped && !ids.iter().any(|existing| existing == id) {
            ids.push(id.to_string());
        }
    }
    // The voice shell is exactly these three. A smaller count means the extractor
    // stopped matching how the page looks elements up, which is the failure this
    // function exists to catch.
    assert_eq!(ids.len(), 3, "expected the page's three elements, found {ids:?}");
    ids
}

#[test]
fn the_voice_page_carries_every_element_the_wasm_client_requires() {
    let rendered = super::templates::voice_page("tww316", true);
    for id in voice_required_ids() {
        assert!(
            rendered.contains(&format!("id=\"{id}\"")),
            "#{id} is looked up by client/src/voice_page.rs but is not in the /voice shell"
        );
    }
}

/// The ids the **WASM** chat page looks up, read out of its source. A second
/// extractor, because the Rust client fetches elements through helpers rather than
/// `requireElement`. Same reason as the two above: a shell that stops carrying an
/// id is a hard failure in the browser, so it has to be a failing test first.
fn wasm_chat_ids() -> Vec<String> {
    let path = std::path::Path::new("../client/src/chat_page.rs");
    let source = std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("{} must be readable: {error}", path.display()));

    let mut ids = Vec::new();
    // Only the three fetch helpers count. Matching on `&document, "` alone also
    // caught `build(&document, "li", …)`, which creates an element rather than
    // looking one up — the contract is about what the page requires to exist.
    for needle in ["element(&document, \"", "button(&document, \"", "dialog(&document, \""] {
        let mut pieces = source.split(needle);
        pieces.next();
        for after in pieces {
            let Some(end) = after.find('"') else { continue };
            let id = &after[..end];
            let shaped = !id.is_empty()
                && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
            if shaped && !ids.iter().any(|existing| existing == id) {
                ids.push(id.to_string());
            }
        }
    }
    assert!(ids.len() >= 6, "only found {ids:?} — the extractor is out of date");
    ids
}

#[test]
fn the_chat_page_carries_every_element_the_wasm_client_requires() {
    let rendered = super::templates::chat_page(&page_fixture(
        &[stored_message()],
        &[ConversationEntry { id: 1, title: "一只红隼".to_string(), is_active: true }],
        Some(1),
        Some(3),
        false,
    ));
    for id in wasm_chat_ids() {
        assert!(
            rendered.contains(&format!("id=\"{id}\"")),
            "#{id} is looked up by client/src/chat_page.rs but is not in the chat shell"
        );
    }
}
