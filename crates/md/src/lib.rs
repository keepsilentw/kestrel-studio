//! The two rendering primitives both sides of the wire need.
//!
//! The server's pages are assembled with `String` (no template engine), and the
//! browser now compiles this same code to WASM. That is the point of the crate:
//! assistant text is markdown on the wire, and whether a turn looks the same
//! watched-live as after a reload depends on **one** renderer deciding it. There
//! were three (Nest SSR with `marked`, the browser with `marked`, this subset),
//! and the shape of a turn differed across them: commit `e7b2911` fixed one such
//! case, and the size of the gap is measured in `docs/verification.md` §11.5.
//!
//! Two properties the tests in `rust/server/src/view/tests.rs` pin, because they are the
//! reason to keep a subset instead of importing a full engine:
//!
//! * everything outside the subset is **escaped, never forwarded**. `marked`
//!   passes raw HTML through, so a model that emits `<script>` gets it executed.
//!   This code cannot, and switching the client onto it closes that hole rather
//!   than opening one.
//! * the escaping happens exactly once, including inside link targets, so
//!   `?a=1&b=2` does not become `&amp;amp;`.
//!
//! A third property belongs to this crate alone: **nesting is capped** (`MAX_NESTING`, 32
//! levels). Rendering quotes, lists and blocks-inside-items is recursion, the input is a
//! model's text, and this copy runs inside the server's own process — where a stack overflow is
//! a process abort that takes every live SSE connection with it, not a failed request. The
//! measurements behind that number, and the catch that plays the same role on the Nest side,
//! are in docs/verification.md §46.7.


/// HTML text/attribute escaping.
///
/// Stricter than the Handlebars default, which leaves `'` alone: the templates
/// interpolate into double-quoted attributes, and a bare apostrophe is only safe
/// as long as nobody rewrites the attribute with single quotes.
pub fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

/// Assistant answers are markdown; the client used to render them with `marked`.
///
/// **This subset is the contract for both online stacks** (docs/roadmap.md §4.1.1 and
/// §4.5): the Nest half now bends `marked` to it instead of following CommonMark, in
/// `nest/server/src/common/markdown.ts` and its browser twin. So the rule is one sentence
/// — inside the subset, render; outside it, show the source, never pass it through.
///
/// In the subset: ATX **and setext** headings, fenced code with a backtick or `~~~` fence
/// (at three columns of indent or less), inline code, bold, italic, strikethrough, inline links,
/// blockquotes
/// (with their lazy continuation line), thematic breaks, GFM tables with `:---` alignment, and
/// lists — `-` / `*` / `+` bullets and `1.` / `1)` ordered items, with indented sublists,
/// `<ol start="N">`, empty items, marker-change-splits-the-list, and loose lists where an item
/// carries several blocks (paragraphs, a nested list, a fence, a quote, a heading).
///
/// Outside it, deliberately: raw HTML (a model can be prompt-injected; `marked` used to forward
/// it), images (assets arrive through the tool pipeline, not through markdown the model writes),
/// task-list checkboxes (no form controls in assistant prose), reference links with their
/// `[label]: url` definitions — footnotes reach the wire as a shortcut reference link, so they
/// are text here too — and **indented code blocks**, because a fenced block is the code shape
/// this product uses; an over-indented run is escaped text, not a code box and not a paragraph
/// of stray code spans either.
///
/// Tables and strikethrough were added on 2026-11-03 (docs/verification.md §15). Everything
/// else in the two paragraphs above was decided on 2026-11-10 by measuring both stacks
/// construct by construct over 80 hand-made samples and closing each divergence in the
/// direction the sentence at the top of this file implies — five rounds of that are in
/// docs/verification.md §46.2, and every round found more than the last one expected.
/// `~~~js` was the worst of them: not unrendered but mis-rendered, because the inline `~~`
/// pairing turned it into `<del>~js …</del>` on the side serving traffic.
///
/// The deliberate divergences from CommonMark are each documented at the helper that
/// implements them: a one-column table must be fenced on both sides, extra cells are
/// dropped rather than inventing a column, and `\|` is the only way to keep a pipe inside
/// a cell.
/// How deep nesting the block loop will follow. The renderer is recursive — a blockquote
/// renders its inside through itself, an item renders its sublist through itself — and it runs
/// **in the server's own process** on text a model produced, which may have been prompt-injected.
/// Measured depth limits, both engines: this crate aborted on a 5,000-level quote at 5,000 and
/// survived 1,200; `marked` threw `RangeError: Maximum call stack size exceeded` between 3,000
/// and 5,000 and survived 3,000 (docs/verification.md §46.6). An abort here kills every live
/// connection, not just the request, so the subset stops at 32 — far above any prose anyone has
/// read on this page (production has zero blockquotes at all, §46.5) — and renders whatever is
/// deeper as the text it was typed as.
const MAX_NESTING: usize = 32;

pub fn markdown(source: &str) -> String {
    render(source, 0)
}

/// The block loop. `depth` is how many levels of quote/list nesting produced this text.
fn render(source: &str, depth: usize) -> String {
    if source.trim().is_empty() {
        return String::new();
    }
    let mut out = String::new();
    let lines: Vec<&str> = source.lines().collect();
    let mut index = 0usize;

    while index < lines.len() {
        let line = lines[index].trim_end();

        // Four spaces of indentation used to mean "indented code block" to CommonMark, and
        // `marked` still makes one out of it. **This subset has no such construct**: a fenced
        // block is the code shape this product uses, so an over-indented run is verbatim text
        // — escaped, and *not* run through the inline pass, which is what keeps an
        // over-indented ```` ``` ```` fence from turning into a paragraph of stray code spans.
        // `renderer.code` in `nest/server/src/common/markdown.ts` bends `marked` to the same
        // rule. The cap is three columns, like CommonMark's own, and it applies at document
        // level only: inside a list item the item's content column governs, which is what
        // keeps `- 甲` with `    - 乙` a nested list.
        if leading_width(lines[index]) > 3 {
            let mut run = vec![line.trim_start()];
            index += 1;
            while index < lines.len() {
                let next = lines[index].trim_end();
                if next.is_empty()
                    || leading_width(lines[index]) <= 3
                    || table_rows(&lines, index).is_some()
                {
                    break;
                }
                run.push(next.trim_start());
                index += 1;
            }
            out.push_str(&format!("<p>{}</p>\n", escape(&run.join("\n"))));
            continue;
        }

        // Fenced code block: whatever is inside is verbatim, so the fence is
        // handled before any inline pass could touch it. Both fence characters are
        // accepted, and the closing run has to be the same character and at least as
        // long as the opening one — `~~~` was the gap this branch used to make worse,
        // because falling through to the paragraph put it into the `~~` strike pass.
        if let Some((marker, length, language)) = fence_marker(line) {
            // The opening fence's own column is the content's: CommonMark and `marked` both
            // cut that much indentation off every body line, so `   ```js` over three lines
            // of indented code does not come out with three stray columns in it.
            let column = leading_width(lines[index]);
            let mut body = Vec::new();
            index += 1;
            while index < lines.len() && !is_closing_fence(lines[index], marker, length) {
                body.push(de_indent(lines[index], column));
                index += 1;
            }
            // Skip the closing fence when there is one.
            if index < lines.len() {
                index += 1;
            }
            let class = if language.is_empty() {
                String::new()
            } else {
                format!(" class=\"language-{}\"", escape(language))
            };
            out.push_str(&format!(
                "<pre><code{class}>{}</code></pre>\n",
                escape(&body.join("\n"))
            ));
            continue;
        }

        if let Some((level, text)) = heading(line) {
            out.push_str(&format!(
                "<h{level}>{}</h{level}>\n",
                inline(text)
            ));
            index += 1;
            continue;
        }

        // A thematic break before the list branch: `- - -` is a rule in CommonMark and in
        // `marked`, and it also matches the `- ` item marker, so order decides it.
        if is_thematic_break(line) {
            out.push_str("<hr>\n");
            index += 1;
            continue;
        }

        if is_quote_line(line) && depth < MAX_NESTING {
            quote_block(&lines, &mut index, &mut out, depth);
            continue;
        }

        if item_mark(line).is_some() && depth < MAX_NESTING {
            list_block(&lines, &mut index, &mut out, depth);
            continue;
        }

        if let Some(end) = table_block(&lines, index, &mut out) {
            index = end;
            continue;
        }

        if line.trim().is_empty() {
            index += 1;
            continue;
        }

        // A paragraph runs to the next blank line, matching how `marked` groups
        // soft-wrapped prose.
        let mut paragraph = vec![line];
        index += 1;
        while index < lines.len() {
            let next = lines[index].trim_end();
            if next.is_empty() {
                break;
            }
            // A run of `=` or `-` under paragraph text is a setext heading, and the text
            // collected so far is its content. This is checked **before** the block-level
            // breaks below, because `---` is also a thematic break and CommonMark (and
            // `marked`) give the heading precedence when a paragraph is open. A spaced
            // `- - -` is not an underline, so it still falls through to the rule.
            if let Some(level) = setext_level(next) {
                out.push_str(&format!(
                    "<h{level}>{}</h{level}>\n",
                    inline(&paragraph.join("\n"))
                ));
                paragraph.clear();
                index += 1;
                break;
            }
            if fence_marker(next).is_some()
                || is_thematic_break(next)
                || is_quote_line(next)
                || heading(next).is_some()
                || item_mark(next).is_some()
                || table_rows(&lines, index).is_some()
            {
                break;
            }
            paragraph.push(next);
            index += 1;
        }
        if !paragraph.is_empty() {
            out.push_str(&format!("<p>{}</p>\n", inline(&paragraph.join("\n"))));
        }
    }

    out.trim_end().to_string()
}

fn heading(line: &str) -> Option<(usize, &str)> {
    // Leading whitespace is not part of the heading itself. It is *cut off here* rather than
    // capped here: the document-level three-column rule lives in the block loop (an
    // over-indented run never reaches this function), while an ATX heading indented under a
    // list item has already been cut back to its own column by `indented_block`. That is how
    // `- 甲 /   # 标题` gets its `<h1>` — the shape docs/verification.md §46.2 round 4 found.
    let line = line.trim_start();
    let level = line.chars().take_while(|c| *c == '#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let rest = &line[level..];
    let text = rest.strip_prefix(' ')?.trim();
    (!text.is_empty()).then_some((level, text))
}

/// The opening line of a fenced code block: the fence character, how long the run is,
/// and the info string.
///
/// Three or more of either character opens. An info string on a backtick fence may not
/// contain a backtick — that is what stops a line of `` `` `inline` `` prose from opening
/// a block that never closes.
fn fence_marker(line: &str) -> Option<(char, usize, &str)> {
    let trimmed = line.trim_start();
    let marker = trimmed.chars().next()?;
    if marker != '`' && marker != '~' {
        return None;
    }
    let length = trimmed.chars().take_while(|c| *c == marker).count();
    if length < 3 {
        return None;
    }
    let info = trimmed[length..].trim();
    if marker == '`' && info.contains('`') {
        return None;
    }
    Some((marker, length, info))
}

/// A closing fence is the same character, at least as long, and nothing else on the line.
fn is_closing_fence(line: &str, marker: char, length: usize) -> bool {
    let trimmed = line.trim();
    let run = trimmed.chars().take_while(|c| *c == marker).count();
    run >= length && trimmed[run..].trim().is_empty()
}

/// `---`, `***`, `___`, with spaces allowed between them. A line that is only a rule.
fn is_thematic_break(line: &str) -> bool {
    let compact: Vec<char> = line.chars().filter(|c| !c.is_whitespace()).collect();
    compact.len() >= 3
        && (compact.iter().all(|c| *c == '-')
            || compact.iter().all(|c| *c == '*')
            || compact.iter().all(|c| *c == '_'))
}

/// The setext underline under a paragraph: `=` makes it an `<h1>`, `-` an `<h2>`.
///
/// Only a run of one character, because `- - -` is a rule and `== ==` is neither.
fn setext_level(line: &str) -> Option<usize> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.chars().all(|c| c == '=') {
        return Some(1);
    }
    if trimmed.chars().all(|c| c == '-') {
        return Some(2);
    }
    None
}

/// Does this line open (or continue) a blockquote? Leading whitespace is not capped:
/// the only way to reach this is from the block loop, where a line that is not markup at
/// all has already become paragraph text.
fn is_quote_line(line: &str) -> bool {
    line.trim_start().starts_with('>')
}

/// A blockquote, rendered by the same block loop so `> > 甲` nests and `> - 甲` is a list.
fn quote_block(lines: &[&str], index: &mut usize, out: &mut String, depth: usize) {
    let mut inner: Vec<String> = Vec::new();
    let mut open_paragraph = false;
    while *index < lines.len() {
        let line = lines[*index].trim_end();
        if let Some(body) = line.trim_start().strip_prefix('>') {
            inner.push(body.strip_prefix(' ').unwrap_or(body).to_string());
            *index += 1;
            // A `>` line carrying text leaves a paragraph open; a bare `>` is a blank line
            // inside the quote, and a blank line is where lazy continuation stops.
            open_paragraph = !body.trim().is_empty();
            continue;
        }
        // Lazy continuation: a plain text line directly under a quoted paragraph line stays
        // **inside** the quote — that is what CommonMark and `marked` do, and leaving it out
        // was a measured divergence (docs/verification.md §46). It stops at a blank line
        // (the block closes there) and at anything that opens a block of its own.
        if !open_paragraph || starts_block_line(lines, *index) {
            break;
        }
        inner.push(line.to_string());
        *index += 1;
    }
    let body = render(&inner.join("\n"), depth + 1);
    if body.is_empty() {
        out.push_str("<blockquote></blockquote>\n");
        return;
    }
    out.push_str(&format!("<blockquote>\n{body}\n</blockquote>\n"));
}

/// Would this line open a block of its own? Used by the lazy-continuation rule above, and
/// it is the same set of things the block loop dispatches on, so the two cannot drift.
fn starts_block_line(lines: &[&str], index: usize) -> bool {
    let Some(line) = lines.get(index).map(|value| value.trim_end()) else {
        return true;
    };
    line.is_empty()
        || fence_marker(line).is_some()
        || heading(line).is_some()
        || is_thematic_break(line)
        || is_quote_line(line)
        || item_mark(line).is_some()
        || table_rows(lines, index).is_some()
}

/// Columns the line is indented by, for list nesting. A tab is four columns.
fn leading_width(line: &str) -> usize {
    let mut width = 0usize;
    for character in line.chars() {
        match character {
            ' ' => width += 1,
            '\t' => width += 4,
            _ => break,
        }
    }
    width
}

/// A list starting at the current line, at whatever column that line's marker sits in.
fn list_block(lines: &[&str], index: &mut usize, out: &mut String, depth: usize) {
    let indent = leading_width(lines[*index]);
    render_list(lines, index, indent, out, depth);
}

/// One list level, at the column its first marker sits in.
///
/// Five rules came out of **measuring** list shapes against `marked` rather than reading the
/// subset back out of the code (docs/verification.md §46.2, the round-3 and round-4 samples):
///
/// 1. a sublist is emitted **inside** the preceding `<li>`, not as a sibling — flattening it
///    is what this function used to do, which put the structure of a nested answer on the
///    table next to the answer itself;
/// 2. a **marker change ends the list**: `- 甲` then `* 乙` is two `<ul>`s;
/// 3. an ordered list **carries its first number** as `<ol start="N">`;
/// 4. a blank line between items makes the list **loose**, and then every item's text is
///    wrapped in a `<p>` — including the items before the gap;
/// 5. an item's own content is a run of blocks: indented prose, a nested list, or a fenced
///    block / blockquote / heading indented under the marker. `- 步骤一` followed by an
///    indented ```` ```bash ```` fence is the shape this matters for: without rule 5 the
///    fence left the list and the list broke in half.
fn render_list(lines: &[&str], index: &mut usize, indent: usize, out: &mut String, depth: usize) {
    let Some(mark) = item_mark(lines[*index].trim_end()) else {
        return;
    };
    let tag = if mark.ordered { "ol" } else { "ul" };
    match mark.number {
        Some(first) if mark.ordered && first != 1 => {
            out.push_str(&format!("<{tag} start=\"{first}\">\n"));
        }
        _ => out.push_str(&format!("<{tag}>\n")),
    }

    let mut items: Vec<Vec<Block>> = Vec::new();
    let mut loose = false;

    while *index < lines.len() {
        let raw = lines[*index];
        let line = raw.trim_end();
        if line.is_empty() {
            // A gap between two items of *this* list loosens it; a gap before prose, or
            // before a shallower line, ends it.
            let continues = lines.get(*index + 1).is_some_and(|next| {
                leading_width(next) == indent && next_mark_continues(next, mark)
            });
            if !continues {
                break;
            }
            loose = true;
            *index += 1;
            continue;
        }
        let Some(current_mark) = item_mark(line) else {
            break;
        };
        if is_thematic_break(line) || !same_list(current_mark, mark) || leading_width(raw) < indent {
            break;
        }

        let column = leading_width(raw) + marker_width(line);
        let mut blocks = vec![Block::Text(inline(&strip_item_marker(line)))];
        *index += 1;
        let mut gap = false;

        loop {
            let Some(raw_child) = lines.get(*index) else { break };
            let child = raw_child.trim_end();
            if child.is_empty() {
                let deeper_follows = lines.get(*index + 1).is_some_and(|next| {
                    leading_width(next) > indent && !next.trim().is_empty()
                });
                if !deeper_follows {
                    break;
                }
                loose = true;
                gap = true;
                *index += 1;
                continue;
            }

            if leading_width(raw_child) > indent {
                if item_mark(child).is_some() && !is_thematic_break(child) {
                    // Past the cap the list stops nesting and the deeper items become siblings
                    // of this one — a flat reading of an answer nobody writes this deep, and
                    // bounded work either way.
                    if depth >= MAX_NESTING {
                        break;
                    }
                    let mut nested = String::new();
                    render_list(lines, index, leading_width(raw_child), &mut nested, depth + 1);
                    blocks.push(Block::Html(nested));
                    continue;
                }
                if starts_block_line(lines, *index) {
                    let mut html = String::new();
                    indented_block(lines, index, indent, column, &mut html, depth);
                    blocks.push(Block::Html(html));
                    continue;
                }
                // A plain indented line: text, with its indentation dropped. `marked` cuts the
                // indentation off an over-indented run for the code block it no longer makes
                // (see `renderer.code` in `nest/server/src/common/markdown.ts`), and keeping
                // the columns here would show them — `.content` renders with `pre-wrap`.
                let text = inline(child.trim_start());
                if gap || matches!(blocks.last(), Some(Block::Html(_))) {
                    blocks.push(Block::Text(text));
                } else if let Some(Block::Text(previous)) = blocks.last_mut() {
                    previous.push('\n');
                    previous.push_str(&text);
                }
                gap = false;
                *index += 1;
                continue;
            }

            // Lazy continuation: prose needs no indent to keep an open paragraph going, at
            // any column — that is CommonMark's rule and `marked` follows it, so
            // `- 甲` then a flush-left line is one item, not a list plus a paragraph.
            if gap || starts_block_line(lines, *index) {
                break;
            }
            if let Some(Block::Text(previous)) = blocks.last_mut() {
                previous.push('\n');
                previous.push_str(&inline(child));
                *index += 1;
            } else {
                break;
            }
        }

        items.push(blocks);
    }

    for blocks in items {
        out.push_str("<li>");
        for block in blocks {
            match block {
                Block::Text(text) if loose => out.push_str(&format!("<p>{text}</p>\n")),
                Block::Text(text) => out.push_str(&text),
                Block::Html(html) => out.push_str(&html),
            }
        }
        out.push_str("</li>\n");
    }

    out.push_str(&format!("</{tag}>\n"));
}

/// One of an item's own blocks: prose that a loose list wraps in a paragraph, or a block
/// that is already HTML and never gets wrapped.
enum Block {
    Text(String),
    Html(String),
}

/// Columns the marker occupies on its own line: `-` plus its following spaces (at least
/// one), or `1.` / `1)` the same way. This is the column indented content lines are cut at.
fn marker_width(line: &str) -> usize {
    let trimmed = line.trim_start();
    let Some(mark) = item_mark(trimmed) else {
        return 0;
    };
    let head = if mark.ordered {
        let digits = trimmed.chars().take_while(|c| c.is_ascii_digit()).count();
        digits + mark.marker.len_utf8()
    } else {
        mark.marker.len_utf8()
    };
    let spaces = trimmed[head..].chars().take_while(|c| *c == ' ').count();
    head + spaces.max(1)
}

/// Remove up to `column` leading spaces — `marked` cuts indented item content the same way,
/// which is what keeps an indented fence from coming out with its bullets' whitespace in it.
fn de_indent(line: &str, column: usize) -> String {
    let spaces = line.chars().take_while(|c| *c == ' ').count().min(column);
    line[spaces..].to_string()
}

/// The run of indented lines that opens a block of its own inside an item — a fence, a
/// quote, a heading, a table — rendered by the same block loop after being cut back.
fn indented_block(
    lines: &[&str],
    index: &mut usize,
    indent: usize,
    column: usize,
    out: &mut String,
    depth: usize,
) {
    let mut run: Vec<String> = Vec::new();
    while *index < lines.len() {
        let raw = lines[*index];
        if raw.trim_end().is_empty() {
            let deeper_follows = lines
                .get(*index + 1)
                .is_some_and(|next| leading_width(next) > indent && !next.trim().is_empty());
            if !deeper_follows {
                break;
            }
            run.push(String::new());
            *index += 1;
            continue;
        }
        if leading_width(raw) <= indent {
            break;
        }
        run.push(de_indent(raw.trim_end(), column));
        *index += 1;
    }
    out.push_str(&render(&run.join("\n"), depth + 1));
    out.push('\n');
}

/// Do two item marks belong to **one** list? The number an ordered item carries does not
/// count: `1. 甲` then `2. 乙` is one list, while `1. 甲` then `2) 乙` is two — `marked`
/// splits on the delimiter, not on the count.
fn same_list(current: ItemMark, list: ItemMark) -> bool {
    current.ordered == list.ordered && current.marker == list.marker
}

/// The same question asked of a line that has not been parsed yet.
fn next_mark_continues(line: &str, list: ItemMark) -> bool {
    item_mark(line.trim_end()).is_some_and(|mark| same_list(mark, list))
}

/// What kind of list item a line is.
#[derive(Clone, Copy, PartialEq, Eq)]
struct ItemMark {
    ordered: bool,
    /// The bullet character (`-` / `*` / `+`) or the ordered delimiter (`.` / `)`).
    ///
    /// A list ends where this changes, and `marked` / CommonMark then start a **new** list.
    /// So `- 甲` followed by `* 乙` is two `<ul>`s rather than one with two items, and the
    /// marker has to be part of the item's identity for that to be one comparison.
    marker: char,
    /// The number an ordered item carries. Only the first item's matters: it becomes
    /// `<ol start="N">`, because dropping it would silently renumber an answer that is
    /// continuing somebody else's list.
    number: Option<u32>,
}

/// All three bullet characters and both ordered delimiters are accepted — they are the same
/// construct with a different marker, and refusing them would not narrow what the page can
/// show, it would only make the serving side print a list as prose. A marker with nothing
/// after it is an **empty item** (`-` on its own line is `<li></li>` in `marked` too), which
/// is the shape that keeps `甲 / - / 乙` from becoming a paragraph of a dash.
fn item_mark(line: &str) -> Option<ItemMark> {
    let trimmed = line.trim_start();
    let first = trimmed.chars().next()?;
    if matches!(first, '-' | '*' | '+') {
        let rest = &trimmed[first.len_utf8()..];
        if !(rest.is_empty() || rest.starts_with(' ') || rest.starts_with('\t')) {
            return None;
        }
        return Some(ItemMark { ordered: false, marker: first, number: None });
    }
    let digits = trimmed.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 || digits > 9 {
        return None;
    }
    let rest = &trimmed[digits..];
    let delimiter = rest.chars().next()?;
    if delimiter != '.' && delimiter != ')' {
        return None;
    }
    let after = &rest[delimiter.len_utf8()..];
    if !(after.is_empty() || after.starts_with(' ') || after.starts_with('\t')) {
        return None;
    }
    let number = trimmed[..digits].parse::<u32>().ok();
    Some(ItemMark { ordered: true, marker: delimiter, number })
}

/// The item's own text, marker removed. Empty items strip down to `""`.
fn strip_item_marker(line: &str) -> String {
    let trimmed = line.trim_start();
    let Some(mark) = item_mark(trimmed) else {
        return trimmed.to_string();
    };
    let cut = if mark.ordered {
        let digits = trimmed.chars().take_while(|c| c.is_ascii_digit()).count();
        digits + mark.marker.len_utf8()
    } else {
        mark.marker.len_utf8()
    };
    trimmed[cut..].trim_start_matches([' ', '\t']).to_string()
}

/// Inline spans: `` `code` `**, `**bold**`, `*italic*`, `[text](url)`.
///
/// Code spans are protected first so a `**` inside one is not styled.
fn inline(text: &str) -> String {
    let mut fragments: Vec<(Fragment, String)> = Vec::new();
    let mut rest = text;

    while let Some(start) = rest.find('`') {
        let (before, after) = (&rest[..start], &rest[start + 1..]);
        match after.find('`') {
            Some(end) => {
                fragments.push((Fragment::Text, before.to_string()));
                fragments.push((Fragment::Code, after[..end].to_string()));
                rest = &after[end + 1..];
            }
            None => break,
        }
    }
    fragments.push((Fragment::Text, rest.to_string()));

    fragments
        .into_iter()
        .map(|(kind, value)| match kind {
            Fragment::Code => format!("<code>{}</code>", escape(&value)),
            Fragment::Text => emphasis(&escape(&value)),
        })
        .collect()
}

enum Fragment {
    Text,
    Code,
}

/// Bold and italic are applied to already-escaped text, so the markers are the
/// only unescaped characters left.
fn emphasis(text: &str) -> String {
    let mut out = text.to_string();
    out = replace_pairs(&out, "**", "<strong>", "</strong>");
    out = replace_pairs(&out, "~~", "<del>", "</del>");
    out = replace_pairs(&out, "*", "<em>", "</em>");
    links(&out)
}

fn replace_pairs(text: &str, marker: &str, open: &str, close: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(marker) {
        // A lone `*` inside a word ("3 * 4") stays as it is.
        let Some(end) = rest[start + marker.len()..].find(marker) else {
            out.push_str(&rest[..start + marker.len()]);
            rest = &rest[start + marker.len()..];
            continue;
        };
        let inner = &rest[start + marker.len()..start + marker.len() + end];
        if inner.trim().is_empty() {
            out.push_str(&rest[..start + marker.len()]);
            rest = &rest[start + marker.len()..];
            continue;
        }
        out.push_str(&rest[..start]);
        out.push_str(open);
        out.push_str(inner);
        out.push_str(close);
        rest = &rest[start + marker.len() + end + marker.len()..];
    }
    out.push_str(rest);
    out
}

/// `[text](url)`. Called on already-escaped text, so neither part is escaped
/// again — doing it would turn `?a=1&b=2` into `&amp;amp;`. Schemes are not
/// filtered: this is the assistant's own prose.
///
/// Two link-shaped things stay text, and both are the subset saying no on purpose:
///
/// * `![alt](url)` — an image. Assets reach the page through the tool pipeline and the
///   `assets` table, not through markdown the model writes (docs/roadmap.md §4.1.1), so
///   embedding an arbitrary URL here would be a fetch the product never agreed to. Before
///   this branch existed the construct collapsed into `<a>` with a stray `!` in front of
///   it, which is neither an image nor text.
/// * `[text][label]` and `[label]` — reference links, and with them footnotes, which
///   arrive as a shortcut reference to a `[label]: url` line. Resolving them needs a
///   document-wide pass the subset does not make; `marked` did resolve them, so the Nest
///   half now turns them back into text as well.
fn links(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let Some(close) = rest[open + 1..].find(']') else { break };
        let label_end = open + 1 + close;
        if !rest[label_end + 1..].starts_with('(') {
            out.push_str(&rest[..open + 1]);
            rest = &rest[open + 1..];
            continue;
        }
        let Some(paren_end) = rest[label_end + 2..].find(')') else { break };
        let url = &rest[label_end + 2..label_end + 2 + paren_end];
        let label = &rest[open + 1..label_end];
        let tail = label_end + 2 + paren_end + 1;
        // `!` is one byte, so `open - 1` is a character boundary.
        if open > 0 && rest.as_bytes()[open - 1] == b'!' {
            out.push_str(&rest[..tail]);
            rest = &rest[tail..];
            continue;
        }
        out.push_str(&rest[..open]);
        out.push_str(&format!("<a href=\"{url}\">{label}</a>"));
        rest = &rest[tail..];
    }
    out.push_str(rest);
    out
}


/// A GFM table: a header row carrying pipes, a `---` delimiter row, then rows that keep
/// the pipes. Emits `marked`'s shape so a turn that streamed past as HTML and the same
/// turn re-rendered after a reload cannot be told apart.
///
/// Returns the index one past the table, or `None` when this is not a table at all.
fn table_block(lines: &[&str], index: usize, out: &mut String) -> Option<usize> {
    let header = lines.get(index)?.trim_end();
    let (aligns, end) = table_rows(lines, index)?;
    let columns = aligns.len();

    out.push_str("<table>\n<thead>\n<tr>");
    for (cell, align) in fit_columns(split_cells(header), columns).iter().zip(&aligns) {
        out.push_str(&format!("<th{}>{}</th>", align.style(), inline(cell.trim())));
    }
    out.push_str("</tr>\n</thead>\n<tbody>\n");
    for row in index + 2..end {
        out.push_str("<tr>");
        for (cell, align) in fit_columns(split_cells(lines[row]), columns).iter().zip(&aligns) {
            out.push_str(&format!("<td{}>{}</td>", align.style(), inline(cell.trim())));
        }
        out.push_str("</tr>\n");
    }
    out.push_str("</tbody>\n</table>\n");
    Some(end)
}

/// The delimiter row's alignment per column, plus the index one past the table.
///
/// The header decides the width: a row with more cells than the delimiter row aligned
/// loses the extras rather than inventing a column with no alignment rule.
fn table_rows(lines: &[&str], index: usize) -> Option<(Vec<Align>, usize)> {
    let header = lines.get(index)?.trim_end();
    if !header.contains('|') {
        return None;
    }
    let aligns = delimiter_cells(lines.get(index + 1)?)?;
    if split_cells(header).len() != aligns.len() {
        return None;
    }
    let mut end = index + 2;
    while end < lines.len() && lines[end].trim_end().contains('|') {
        end += 1;
    }
    Some((aligns, end))
}

#[derive(Clone, Copy, PartialEq)]
enum Align {
    None,
    Left,
    Center,
    Right,
}

impl Align {
    /// Inline style, the way `marked` renders an aligned cell.
    fn style(self) -> &'static str {
        match self {
            Align::None => "",
            Align::Left => " style=\"text-align:left\"",
            Align::Center => " style=\"text-align:center\"",
            Align::Right => " style=\"text-align:right\"",
        }
    }
}

/// `---`, `:---`, `---:`, `:---:` — exactly one per column, no exceptions.
///
/// This row is what tells a table apart from prose that happens to contain a pipe, so
/// it is strict. A **one-column** table additionally has to be fenced on both sides
/// (`| a |` over `| - |`): that is GFM's own disambiguation rule, and without it a line
/// like `2 | 3` would open a table.
fn delimiter_cells(line: &str) -> Option<Vec<Align>> {
    let trimmed = line.trim();
    let cells = split_cells(line);
    if cells.is_empty() {
        return None;
    }
    if cells.len() == 1 && !(trimmed.starts_with('|') && trimmed.ends_with('|')) {
        return None;
    }
    let mut out = Vec::with_capacity(cells.len());
    for cell in &cells {
        let body = cell.trim();
        let left = body.starts_with(':');
        let right = body.len() > 1 && body.ends_with(':');
        let core = body.trim_start_matches(':').trim_end_matches(':');
        if core.is_empty() || !core.chars().all(|c| c == '-') {
            return None;
        }
        out.push(match (left, right) {
            (true, true) => Align::Center,
            (true, false) => Align::Left,
            (false, true) => Align::Right,
            _ => Align::None,
        });
    }
    Some(out)
}

/// Split a row on **unescaped** pipes, dropping the row's own outer pipes.
///
/// `\|` is content, not a separator — GFM's rule too, and the documented way to keep a
/// pipe inside a cell. The split happens before any inline pass, so it cannot see code
/// spans: an unescaped `|` inside backticks still splits the cell, and the reference
/// implementation behaves the same way. Escape it instead.
fn split_cells(line: &str) -> Vec<String> {
    let trimmed = line.trim();
    let mut cells: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut chars = trimmed.chars().peekable();
    while let Some(character) = chars.next() {
        match character {
            '\\' if chars.peek() == Some(&'|') => {
                chars.next();
                current.push('|');
            }
            '|' => cells.push(std::mem::take(&mut current)),
            other => current.push(other),
        }
    }
    cells.push(std::mem::take(&mut current));
    if trimmed.starts_with('|') && !cells.is_empty() {
        cells.remove(0);
    }
    if trimmed.ends_with('|') && !cells.is_empty() {
        cells.pop();
    }
    cells
}

/// Force a row to the header's width: missing cells become empty (GFM), extra ones go.
fn fit_columns(cells: Vec<String>, columns: usize) -> Vec<String> {
    let mut row = cells;
    row.truncate(columns);
    while row.len() < columns {
        row.push(String::new());
    }
    row
}
