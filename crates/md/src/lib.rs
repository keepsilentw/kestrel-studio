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
/// Only a subset is implemented here, deliberately: ATX headings, fenced code,
/// inline code, bold, italic, strikethrough, links, `-` / `1.` lists, and GFM tables
/// with `:---` alignment. Everything outside the subset is still escaped, never
/// passed through — unlike `marked`, which forwards raw HTML, so a model that decides
/// to emit `<script>` gets it rendered. This build cannot, and that asymmetry is
/// intentional.
///
/// Tables and strikethrough were added on 2026-11-03. They had been the subset's only
/// known gap — and 49 stored assistant answers used neither (docs/verification.md §15),
/// so they were closed because the gap was a live behavioural difference against
/// `marked` after the cutover, not because anything asked for them. The deliberate
/// divergences from `marked` are each documented at the helper that implements them:
/// a one-column table must be fenced on both sides, extra cells are dropped rather than
/// inventing a column, and `\|` is the only way to keep a pipe inside a cell.
pub fn markdown(source: &str) -> String {
    if source.trim().is_empty() {
        return String::new();
    }
    let mut out = String::new();
    let lines: Vec<&str> = source.lines().collect();
    let mut index = 0usize;

    while index < lines.len() {
        let line = lines[index].trim_end();

        // Fenced code block: whatever is inside is verbatim, so the fence is
        // handled before any inline pass could touch it.
        if let Some(fence) = line.strip_prefix("```") {
            let language = fence.trim();
            let mut body = Vec::new();
            index += 1;
            while index < lines.len() && !lines[index].trim_end().starts_with("```") {
                body.push(lines[index]);
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

        if item_kind(line).is_some() {
            let ordered = item_kind(line) == Some(true);
            let tag = if ordered { "ol" } else { "ul" };
            out.push_str(&format!("<{tag}>\n"));
            while index < lines.len() && item_kind(lines[index].trim_end()).is_some() {
                let text = strip_item_marker(lines[index].trim_end());
                out.push_str(&format!("<li>{}</li>\n", inline(&text)));
                index += 1;
            }
            out.push_str(&format!("</{tag}>\n"));
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
            if next.is_empty()
                || next.starts_with("```")
                || heading(next).is_some()
                || item_kind(next).is_some()
                || table_rows(&lines, index).is_some()
            {
                break;
            }
            paragraph.push(next);
            index += 1;
        }
        out.push_str(&format!("<p>{}</p>\n", inline(&paragraph.join("\n"))));
    }

    out.trim_end().to_string()
}

fn heading(line: &str) -> Option<(usize, &str)> {
    let level = line.chars().take_while(|c| *c == '#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let rest = &line[level..];
    let text = rest.strip_prefix(' ')?.trim();
    (!text.is_empty()).then_some((level, text))
}

/// `Some(true)` for `1.` items, `Some(false)` for `-` / `*` items, `None` for
/// anything else.
fn item_kind(line: &str) -> Option<bool> {
    let trimmed = line.trim_start();
    if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
        return Some(false);
    }
    let digits = trimmed.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 && trimmed.get(digits..digits + 2) == Some(". ") {
        return Some(true);
    }
    None
}

fn strip_item_marker(line: &str) -> String {
    let trimmed = line.trim_start();
    if let Some(rest) = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")) {
        return rest.to_string();
    }
    let digits = trimmed.chars().take_while(|c| c.is_ascii_digit()).count();
    trimmed[digits + 2..].to_string()
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
/// again — doing it twice would turn `?a=1&b=2` into `&amp;amp;`. Schemes are not
/// filtered: this is the assistant's own prose.
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
        out.push_str(&rest[..open]);
        out.push_str(&format!("<a href=\"{url}\">{label}</a>"));
        rest = &rest[label_end + 2 + paren_end + 1..];
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
