/// The rendering primitives the templates need.
///
/// There is no template engine in this build (`handlebars`, `tera` and
/// `minijinja` are all absent from the local registry), so the pages are
/// assembled here with `String`. Two things every template gets from this module
/// rather than doing by hand: escaping, and the markdown subset the assistant's
/// answers are stored in.

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

/// Assistant answers are markdown; the client renders them with `marked`.
///
/// Only a subset is implemented here, deliberately: ATX headings, fenced code,
/// inline code, bold, italic, links, and `-` / `1.` lists. Everything outside the
/// subset is still escaped, never passed through — unlike `marked`, which forwards
/// raw HTML, so a model that decides to emit `<script>` gets it rendered. This
/// build cannot, and that asymmetry is intentional.
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
