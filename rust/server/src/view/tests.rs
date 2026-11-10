//! The template / client-script contract, and the first-paint behaviour.
//!
//! Two independent halves of the guarantee the Nest suite encoded:
//!
//! 1. the ids the page must carry are **read out of the client source**, not
//!    restated here, so editing `nest/client/scripts/main.ts` to require a new element
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
    let path = std::path::Path::new("../../nest/client/scripts/main.ts");
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
        // The fixture renders what production renders by default; the switch has its
        // own test and builds the page with `..`-update instead.
        client: crate::config::ClientBundle::Nest,
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
            crate::conversation::repo::AssetView {
                id: 7,
                kind: TaskKind::Image,
                url: "/api/v1/assets/7/download".to_string(),
                mime: "image/png".to_string(),
                bytes: 4,
                // Unknown on purpose: this fixture is what pins that a row without
                // dimensions renders the *same* `<img>` as before the columns existed.
                width: None,
                height: None,
            },
            crate::conversation::repo::AssetView {
                id: 8,
                kind: TaskKind::Video,
                url: "/api/v1/assets/8/download".to_string(),
                mime: "video/mp4".to_string(),
                bytes: 8,
                width: None,
                height: None,
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
        client: crate::config::ClientBundle::Nest,
    };
    let rendered = super::templates::chat_page(&page);

    for id in required_ids() {
        assert!(
            rendered.contains(&format!("id=\"{id}\"")),
            "the client requires #{id}, the template does not render it"
        );
    }
}

/// The shortcut hint moved inside the input box: the CSS only paints it while the
/// textarea has focus (`.composer-input:focus ~ .composer-hint`), and that sibling
/// selector is only true when the two are in the same `.composer-field`. A template
/// that drifts back to a footer row loses the hint with no console error at all,
/// so the position is pinned here instead. The Nest template is the same shape; its
/// counterpart is nest/server/__tests__/view/view.controller.test.ts.
#[test]
fn the_shortcut_hint_sits_inside_the_input_box() {
    let rendered = super::templates::chat_page(&page_fixture(&[], &[], Some(4), Some(12), false));
    // The logout form in the header also closes a <form>, so measure from the
    // composer's own opening tag.
    let composer = rendered.find("id=\"composer-form\"").expect("the composer is rendered");
    let field = rendered[composer..].find("class=\"composer-field\"").expect("the field wraps the textarea") + composer;
    let hint = rendered[composer..].find("class=\"composer-hint\"").expect("the hint is rendered") + composer;
    let form_end = rendered[composer..].find("</form>").expect("the composer form closes") + composer;

    assert!(field < hint, "the hint must be inside .composer-field, not below it: {rendered}");
    assert!(hint < form_end, "the hint must be inside the form: {rendered}");
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
fn a_lazy_image_with_known_dimensions_carries_the_box() {
    // The reflow fix, and the reason the numbers come from the file's header: the
    // reserved box is only right if the pair is right, and a lone `width` is worse
    // than nothing because the browser keeps its own default ratio.
    let mut view = stored_message();
    view.assets[0].width = Some(1024);
    view.assets[0].height = Some(576);
    let rendered = super::templates::turn(&render_messages(vec![view]).remove(0));

    assert!(
        rendered.contains(
            "<img src=\"/api/v1/assets/7/download\" width=\"1024\" height=\"576\" alt=\"生成的图片\" loading=\"lazy\" class=\"asset-image\">"
        ),
        "{rendered}"
    );
    // The video is untouched: this probe reads image headers, and `<video>` gets its
    // intrinsic size from `preload="metadata"` instead.
    assert!(rendered.contains("<video src=\"/api/v1/assets/8/download\" controls"));

    // Half a pair is not a box.
    let mut half = stored_message();
    half.assets[0].width = Some(1024);
    let rendered = super::templates::turn(&render_messages(vec![half]).remove(0));
    assert!(
        !rendered.contains("width="),
        "one dimension alone is left off, not guessed at"
    );
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
    assert_eq!(markdown("~~旧值~~"), "<p><del>旧值</del></p>");
    assert_eq!(
        markdown("| 名字 | 分数 |\n| :-- | ---: |\n| 阿隼 | 98 |"),
        "<table>\n<thead>\n<tr><th style=\"text-align:left\">名字</th><th style=\"text-align:right\">分数</th></tr>\n</thead>\n<tbody>\n<tr><td style=\"text-align:left\">阿隼</td><td style=\"text-align:right\">98</td></tr>\n</tbody>\n</table>",
        "GFM table with per-column alignment, rendered the way marked does it"
    );
}

/// The table subset's deliberate limits, pinned as behaviour rather than comment.
///
/// Each one is a place where this renderer and `marked` can disagree on purpose, so a
/// future edit that "fixes" one of them has to say why out loud.
#[test]
fn the_table_rules_that_keep_prose_from_turning_into_a_table() {
    // A pipe in prose is not a table; only the delimiter row makes one.
    assert_eq!(markdown("名字 | 分数"), "<p>名字 | 分数</p>");
    assert_eq!(markdown("2 | 3"), "<p>2 | 3</p>");
    assert_eq!(
        markdown("名字 | 分数\n阿隼 | 98"),
        "<p>名字 | 分数\n阿隼 | 98</p>",
        "two pipe rows still need the --- line between them; soft-wrapped prose stays one paragraph"
    );

    // One column is only a table when fenced on both sides (GFM's own rule).
    assert_eq!(markdown("a\n| - |"), "<p>a\n| - |</p>", "a lone delimiter row is not a table either");
    assert_eq!(
        markdown("| a |\n| - |\n| **b** |"),
        "<table>\n<thead>\n<tr><th>a</th></tr>\n</thead>\n<tbody>\n<tr><td><strong>b</strong></td></tr>\n</tbody>\n</table>"
    );

    // The header fixes the width: extra cells go, missing ones become empty.
    let rows = markdown("| a | b |\n| - | - |\n| 1 | 2 | 3 |\n| 4 |");
    assert!(rows.contains("<tr><td>1</td><td>2</td></tr>"), "{rows}");
    assert!(rows.contains("<tr><td>4</td><td></td></tr>"), "{rows}");

    // `\|` is the documented way to keep a pipe inside a cell…
    let escaped = markdown("| a | b |\n| - | - |\n| x \\| y | z |");
    assert!(escaped.contains("<td>x | y</td>"), "{escaped}");
    // …and an unescaped one inside a code span still splits, because the split runs
    // before any inline pass and cannot see code spans. The reference implementation
    // behaves the same way; the leftover backticks stay literal text, which is the
    // visible cost of not escaping.
    let split = markdown("| a | b |\n| - | - |\n| `x | y` |");
    assert!(split.contains("<tr><td>`x</td><td>y`</td></tr>"), "{split}");

    // The subset's core property survives the new block: cells are escaped, not
    // forwarded, even inside a table.
    let unsafe_cell = markdown("| a |\n| - |\n| <script>x</script> |");
    assert!(!unsafe_cell.contains("<script>"), "{unsafe_cell}");
    assert!(unsafe_cell.contains("&lt;script&gt;"), "{unsafe_cell}");

    // A table interrupts a paragraph; the row before it stays its own block.
    let after = markdown("说明文字\n| a | b |\n| - | - |\n| 1 | 2 |");
    assert!(after.starts_with("<p>说明文字</p>"), "{after}");
    assert!(after.contains("<table>"), "{after}");
}

/// The block constructs the two online stacks disagreed about.
///
/// Measured construct by construct on 2026-11-09 (docs/verification.md §36.2) and closed on
/// 2026-11-10 by moving them into the subset here, rather than by bending `marked` further
/// away from what an answer looks like. Each shape below is the one `marked` produced,
/// re-measured in the same run — the point of the exercise is that nobody has to remember
/// which side was right.
#[test]
fn the_block_constructs_that_used_to_reach_users_as_text() {
    // `~~~` fences. Before this the opening line fell through to the paragraph, where the
    // strike pass paired `~~` and the code came out as `<del>~js console.log(1)</del>` —
    // worse than unrendered, and it was the version on the wire.
    assert_eq!(
        markdown("~~~js\nconsole.log(1)\n~~~"),
        "<pre><code class=\"language-js\">console.log(1)</code></pre>"
    );
    assert_eq!(
        markdown("~~~\nplain\n~~~"),
        "<pre><code>plain</code></pre>",
        "a tilde fence with no info string carries no class, as `` ` `` does not either"
    );
    assert_eq!(
        markdown("~~旧值~~"),
        "<p><del>旧值</del></p>",
        "two tildes are still strikethrough, not a fence"
    );

    // Blockquotes, nested the same way by recursion.
    assert_eq!(
        markdown("> 引用一行"),
        "<blockquote>\n<p>引用一行</p>\n</blockquote>"
    );
    assert_eq!(
        markdown("> 甲\n> 乙"),
        "<blockquote>\n<p>甲\n乙</p>\n</blockquote>",
        "quoted lines stay one soft-wrapped paragraph"
    );
    assert_eq!(
        markdown("> > 甲"),
        "<blockquote>\n<blockquote>\n<p>甲</p>\n</blockquote>\n</blockquote>"
    );
    assert_eq!(markdown(">"), "<blockquote></blockquote>");

    // Thematic breaks, including the spaced form that also matches an item marker.
    assert_eq!(markdown("---"), "<hr>");
    assert_eq!(markdown("***"), "<hr>");
    assert_eq!(markdown("___"), "<hr>");
    assert_eq!(markdown("- - -"), "<hr>");

    // Setext headings, and the rule about which of the two `---` readings wins.
    assert_eq!(markdown("标题\n==="), "<h1>标题</h1>");
    assert_eq!(markdown("标题\n---"), "<h2>标题</h2>");
    assert_eq!(
        markdown("文本\n- - -"),
        "<p>文本</p>\n<hr>",
        "a spaced rule under text is a rule, not a level-2 heading"
    );
    assert_eq!(
        markdown("甲\n\n---\n\n乙"),
        "<p>甲</p>\n<hr>\n<p>乙</p>",
        "blank lines on both sides make it a rule again"
    );

    // Nested lists, emitted inside the preceding item rather than flattened beside it.
    assert_eq!(
        markdown("- 甲\n  - 乙\n- 丙"),
        "<ul>\n<li>甲<ul>\n<li>乙</li>\n</ul>\n</li>\n<li>丙</li>\n</ul>"
    );
    assert_eq!(
        markdown("1. 甲\n   1. 乙"),
        "<ol>\n<li>甲<ol>\n<li>乙</li>\n</ol>\n</li>\n</ol>",
        "ordered lists nest too, at a column of three because their marker is three wide"
    );
    assert_eq!(
        markdown("- 甲\n- 乙"),
        "<ul>\n<li>甲</li>\n<li>乙</li>\n</ul>",
        "a flat list is unchanged by all of this"
    );
}

/// What the subset still refuses, now refused **the same way on both stacks**.
///
/// These stay text on purpose; the fix was making `marked` agree, not adding them here.
/// The reasoning per construct is in `crates/md/src/lib.rs` and docs/roadmap.md §4.1.1.
#[test]
fn what_the_subset_keeps_out_and_shows_as_source() {
    // Images: assets come from the tool pipeline, and this used to collapse into a link
    // with a stray `!` in front of it.
    assert_eq!(
        markdown("![红隼](https://example.test/a.png)"),
        "<p>![红隼](https://example.test/a.png)</p>"
    );
    assert_eq!(
        markdown("图 ![一张](https://example.test/a.png) 完"),
        "<p>图 ![一张](https://example.test/a.png) 完</p>",
        "and the prose around it survives"
    );

    // Task-list checkboxes: no form controls in assistant prose.
    assert_eq!(
        markdown("- [ ] 甲\n- [x] 乙"),
        "<ul>\n<li>[ ] 甲</li>\n<li>[x] 乙</li>\n</ul>"
    );

    // Reference links, and footnotes, which arrive as a shortcut reference to a
    // definition line. Both halves of the construct stay where they were typed.
    assert_eq!(
        markdown("有脚注[^1]。\n\n[^1]: 说明一行"),
        "<p>有脚注[^1]。</p>\n<p>[^1]: 说明一行</p>"
    );
    assert_eq!(
        markdown("见 [文档][ref]。\n\n[ref]: https://example.test/x?a=1&b=2"),
        "<p>见 [文档][ref]。</p>\n<p>[ref]: https://example.test/x?a=1&amp;b=2</p>",
        "escaped once, including inside a line that is not a link here"
    );

    // An inline link is still a link — the refusal above is about the reference forms.
    assert_eq!(
        markdown("[红隼](https://example.test?a=1&b=2)"),
        "<p><a href=\"https://example.test?a=1&amp;b=2\">红隼</a></p>"
    );
}

/// Marker spellings, lazy continuation, and the five list rules that only a widened sample
/// set found. The sample set grew 19 → 72 across four rounds (docs/verification.md §46.2),
/// and every round produced divergences the previous round could not have seen: this file is
/// the reason those readings are pinned rather than described.
#[test]
fn the_marker_spellings_and_the_lazy_continuation_line() {
    assert_eq!(markdown("+ 甲\n+ 乙"), "<ul>\n<li>甲</li>\n<li>乙</li>\n</ul>");
    assert_eq!(markdown("1) 甲\n2) 乙"), "<ol>\n<li>甲</li>\n<li>乙</li>\n</ol>");
    assert_eq!(
        markdown("- 甲\n  + 乙"),
        "<ul>\n<li>甲<ul>\n<li>乙</li>\n</ul>\n</li>\n</ul>",
        "a different bullet character at a deeper column still nests"
    );
    assert_eq!(markdown("- 甲\n\n  继续一段\n\n- 乙"), {
        // a loose list, with a second paragraph inside the first item
        "<ul>\n<li><p>甲</p>\n<p>继续一段</p>\n</li>\n<li><p>乙</p>\n</li>\n</ul>"
    });

    // The line under a quoted paragraph belongs to the quote; a blank line closes it.
    assert_eq!(
        markdown("> 甲\n乙"),
        "<blockquote>\n<p>甲\n乙</p>\n</blockquote>"
    );
    assert_eq!(
        markdown("> 甲\n>\n> 乙"),
        "<blockquote>\n<p>甲</p>\n<p>乙</p>\n</blockquote>",
        "a bare `>` is a blank line inside the quote, not a continuation"
    );
    assert_eq!(
        markdown("> 甲\n- 乙"),
        "<blockquote>\n<p>甲</p>\n</blockquote>\n<ul>\n<li>乙</li>\n</ul>",
        "a line that opens a block of its own is not lazy text"
    );
    // …and prose that needs no indent at all still continues a list item (CommonMark's rule,
    // which `marked` follows): without this the list broke in half.
    assert_eq!(markdown("- 甲\n后面一段"), "<ul>\n<li>甲\n后面一段</li>\n</ul>");
}

/// The list identity rules, each one taken from the same run of both engines over the same
/// sample (docs/verification.md §46.2, rounds 3 and 4).
#[test]
fn the_list_identity_rules_the_samples_found() {
    // A bullet character change ends the list; the number an ordered item carries does not.
    assert_eq!(
        markdown("* 甲\n- 乙"),
        "<ul>\n<li>甲</li>\n</ul>\n<ul>\n<li>乙</li>\n</ul>",
        "two lists, as marked makes them"
    );
    assert_eq!(
        markdown("1. 甲\n2) 乙"),
        "<ol>\n<li>甲</li>\n</ol>\n<ol start=\"2\">\n<li>乙</li>\n</ol>",
        "the delimiter is part of a list's identity, the count is not"
    );
    assert_eq!(
        markdown("1. 甲\n2. 乙\n3. 丙"),
        "<ol>\n<li>甲</li>\n<li>乙</li>\n<li>丙</li>\n</ol>"
    );

    // `<ol start="N">` when the list does not open on 1 — dropping it would renumber an
    // answer that is continuing somebody else's list.
    assert_eq!(
        markdown("2. 甲\n3. 乙"),
        "<ol start=\"2\">\n<li>甲</li>\n<li>乙</li>\n</ol>"
    );

    // A lone marker is an empty item, not a paragraph about a dash.
    assert_eq!(
        markdown("甲\n\n-\n\n乙"),
        "<p>甲</p>\n<ul>\n<li></li>\n</ul>\n<p>乙</p>"
    );

    // A gap between items loosens the whole list, including the items before the gap.
    assert_eq!(
        markdown("- 甲\n\n- 乙"),
        "<ul>\n<li><p>甲</p>\n</li>\n<li><p>乙</p>\n</li>\n</ul>"
    );
    assert_eq!(
        markdown("- 甲\n- 乙"),
        "<ul>\n<li>甲</li>\n<li>乙</li>\n</ul>",
        "no gap, no paragraphs"
    );

    // Content indented under a marker stays **inside** that item, including a fence — which
    // is the shape an answer uses for a command per step.
    assert_eq!(
        markdown("- 步骤一\n  ```bash\n  npm i\n  ```\n- 步骤二"),
        "<ul>\n<li>步骤一<pre><code class=\"language-bash\">npm i</code></pre>\n</li>\n<li>步骤二</li>\n</ul>"
    );
    assert_eq!(
        markdown("- 甲\n  > 引用"),
        "<ul>\n<li>甲<blockquote>\n<p>引用</p>\n</blockquote>\n</li>\n</ul>"
    );
    assert_eq!(
        markdown("- 甲\n  # 标题"),
        "<ul>\n<li>甲<h1>标题</h1>\n</li>\n</ul>",
        "an indented ATX heading is still a heading, at any column this subset reaches"
    );
}

/// Four columns of indentation is an indented code block to CommonMark. This subset has no
/// such construct — a fenced block is the code shape this product uses — so an over-indented
/// run is verbatim text, and `renderer.code` in `nest/server/src/common/markdown.ts` bends
/// `marked` to the same rule (docs/verification.md §46.2, round 5).
#[test]
fn an_over_indented_run_is_text_not_a_code_block() {
    assert_eq!(
        markdown("段落\n\n    四格缩进的一行\n\n结束"),
        "<p>段落</p>\n<p>四格缩进的一行</p>\n<p>结束</p>"
    );
    assert_eq!(markdown("    - 甲\n    - 乙"), "<p>- 甲\n- 乙</p>");
    assert_eq!(
        markdown("    ```js\n    let x = 1;\n    ```"),
        "<p>```js\nlet x = 1;\n```</p>",
        "and the run is escaped verbatim, not run through the inline pass — otherwise an \
         over-indented fence becomes a paragraph of stray code spans"
    );
    assert_eq!(
        markdown("\t一个制表符开头"),
        "<p>一个制表符开头</p>",
        "a tab is four columns, so it is the same case"
    );
    assert_eq!(
        markdown("- 甲\n\n      更深六格"),
        "<ul>\n<li><p>甲</p>\n<p>更深六格</p>\n</li>\n</ul>",
        "inside an item the over-indent is dropped the same way"
    );
    assert_eq!(
        markdown("   ```js\n   let x = 1;\n   ```"),
        "<pre><code class=\"language-js\">let x = 1;</code></pre>",
        "three columns is still a fence"
    );
}

/// The nesting cap, and why a cap belongs in a renderer at all: this code runs inside the
/// server's own process on text a model produced, and a model can be prompt-injected. Measured
/// on the same build (docs/verification.md §46.6): uncapped, a line of 5,000 `>` aborted the
/// process with a stack overflow — which kills every live SSE connection, not one request.
/// `marked` on the same input throws a catchable `RangeError` between 3,000 and 5,000 levels.
#[test]
fn nesting_stops_at_thirty_two_levels() {
    let source = format!("{} 深", ">".repeat(4_000));
    let deep = markdown(&source);

    assert_eq!(deep.matches("<blockquote>").count(), 32, "{deep}");
    assert!(deep.contains("&gt;"), "the markers past the cap stay the text they were typed as");

    let lines: String =
        (1..=400usize).map(|level| format!("{}- 层{level}\n", " ".repeat(level * 2))).collect();
    let lists = markdown(&lines);

    assert!(lists.matches("<ul>").count() <= 33, "{}", lists.matches("<ul>").count());
    assert_eq!(lists.matches("<li>").count(), 400, "the work stays linear once nesting stops");
}

/// The document that closes the gap, pinned in **three** places: here,
/// `nest/server/__tests__/common/markdown.test.ts` and
/// `nest/client/__tests__/markdown.test.ts`. One string, three renderers, so a change to
/// any of the three shows up as a failure somewhere rather than as two pages that disagree
/// in production (docs/verification.md §46 is the reading that made this worth pinning).
#[test]
fn one_document_renders_identically_through_both_stacks() {
    let source = "![隼](https://example.test/a.png)\n\n> 引用一行\n\n- 甲\n  - 乙\n\n文本 **粗** [文档][ref]\n\n[ref]: https://example.test/x?a=1&b=2";

    assert_eq!(
        markdown(source),
        "<p>![隼](https://example.test/a.png)</p>\n\
         <blockquote>\n<p>引用一行</p>\n</blockquote>\n\
         <ul>\n<li>甲<ul>\n<li>乙</li>\n</ul>\n</li>\n</ul>\n\
         <p>文本 <strong>粗</strong> [文档][ref]</p>\n\
         <p>[ref]: https://example.test/x?a=1&amp;b=2</p>"
    );
}

/// The ids the WASM voice page looks up, read out of **its own source** — the
/// same device [`required_ids`] uses for the chat page and `nest/client/scripts/main.ts`.
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
    let rendered = super::templates::voice_page("tww316", true, crate::config::ClientBundle::Nest);
    for id in voice_required_ids() {
        assert!(
            rendered.contains(&format!("id=\"{id}\"")),
            "#{id} is looked up by rust/client/src/voice_page.rs but is not in the /voice shell"
        );
    }
}


/// `CLIENT_BUNDLE` is the whole switch, so it is asserted where it takes effect: the
/// rendered tag. `/` and `/voice` are the only two pages that load a client at all
/// (`/admin` deliberately loads none), and one page never carries both clients.
#[test]
fn the_client_switch_hands_each_page_exactly_one_client() {
    use crate::config::ClientBundle;

    let nest = super::templates::chat_page(&page_fixture(&[], &[], None, None, false));
    assert!(nest.contains("<script type=\"module\" src=\"/assets/main.js\"></script>"), "{nest}");
    assert!(!nest.contains("kestrel_client.js"), "the default must not load WASM");

    let wasm = super::templates::chat_page(&ChatPage {
        client: ClientBundle::Wasm,
        ..page_fixture(&[], &[], None, None, false)
    });
    assert!(wasm.contains("import(\"/wasm/kestrel_client.js\")"), "{wasm}");
    assert!(wasm.contains("client.initChat()"), "{wasm}");
    assert!(!wasm.contains("src=\"/assets/main.js\""), "one client per page: {wasm}");

    let voice = super::templates::voice_page("tww316", false, ClientBundle::Wasm);
    assert!(voice.contains("client.initVoice()"), "{voice}");
    // Tag-shaped on purpose: the loader's own failure note names the legacy bundle, so
    // a bare path in the page is not evidence of a second client.
    assert!(!voice.contains("src=\"/assets/voice.js\""), "no TS client tag on the voice page: {voice}");
    assert!(
        voice.contains("import(\"/wasm/kestrel_client.js\")"),
        "both pages must resolve the same module, or one of them is broken by a typo: {voice}"
    );

    // The worklet is still JS at that exact path — `rust/client/src/audio.rs` asks for
    // `/capture.js` and no amount of WASM changes that (docs/roadmap.md §4.1, the JS floor).
    assert!(
        std::path::Path::new("../client/public/capture.js").exists(),
        "audio.rs loads /capture.js; the source file it is built from moved"
    );
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
            "#{id} is looked up by rust/client/src/chat_page.rs but is not in the chat shell"
        );
    }
}
