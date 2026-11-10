import { Marked, type Token, type Tokens } from 'marked';

/**
 * The markdown renderer, with raw HTML turned back into text.
 *
 * Assistant text is model output, and a model can be prompt-injected. `marked` with
 * its default options puts any HTML in that text **into the DOM verbatim** (v5 removed
 * the old `sanitize` option, and the official advice is a DOM sanitizer), while the
 * Rust side of this repository renders the same text through `crates/md`, whose whole
 * point is that anything outside its subset is escaped and never passed through. Two
 * online implementations must not hold two different content-injection policies —
 * docs/roadmap.md §4.5, and the per-construct measurement in docs/verification.md §36.2
 * is what turned this from a suspicion into a reading.
 *
 * Which hook does the job is not obvious, and guessing costs a release:
 * `tokenizer.html` returning `false` means "fall back to the default tokenization"
 * (verified against marked 18: returning `false`, or `undefined`, changes nothing at
 * all). The renderer is where the decision lands — marked calls `html(token)` once per
 * HTML token with the literal source in `token.raw`, so escaping there is exactly the
 * "show the source" behaviour `kestrel-md` has.
 *
 * Duplication note: `nest/client/scripts/markdown.ts` is the same configuration for the
 * browser bundle. The two trees are split by backend stack on purpose (each stays
 * independently buildable), and there is no shared JS package — creating one would mean
 * touching `pnpm-workspace.yaml` and the lockfile. Keep the two in step by hand, the way
 * the two DDL strings already are.
 */

/**
 * One subset, two engines.
 *
 * `crates/md` is the contract for both online stacks (docs/roadmap.md §4.1.1, §4.5), so
 * here `marked` is bent to it rather than the subset grown to CommonMark. The constructs
 * that had to be turned back into text are the ones the subset refuses on purpose:
 *
 * * **images** — assets reach the page through the tool pipeline and the `assets` table,
 *   never through markdown the model writes;
 * * **task-list checkboxes** — assistant prose is not a form;
 * * **reference links and their `[label]: url` definitions** — resolving them needs a
 *   document-wide pass the subset does not make, and footnotes reach the wire as a
 *   shortcut reference link, so `[^1]` goes back to text with them.
 *
 * Every one of those was measured before it was chosen, per construct, against the same
 * samples on both engines: docs/verification.md §36.2 found nine divergences, §38.3 closed
 * the raw-HTML one, and §46 closes these. The five block constructs that disagreed the
 * other way — blockquote, thematic break, setext heading, nested list, `~~~` fence — went
 * **into** the subset instead, because there the text-looking side was the broken one:
 * `~~~js` was rendering as `<del>~js …</del>`.
 */

/**
 * The same five characters `escape()` in `crates/md` escapes, in the same spelling — so a run
 * of text that came out of the Rust renderer and one that came out of here are byte-equal, not
 * merely equivalent. Handlebars would leave the apostrophe alone; this does not, and neither
 * does the Rust side.
 */
function escapeHtml(value: string): string {
  return value
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

/** A token that carries nothing but the source it was written from. */
function literalToken(raw: string): Tokens.Text {
  return { type: 'text', raw, text: raw };
}

/** Child token slots, per token type. `marked` keeps them under four different keys. */
function childSlots(token: Token): Token[][] {
  const slots: Token[][] = [];
  if ('tokens' in token && Array.isArray(token.tokens)) {
    slots.push(token.tokens);
  }
  if ('items' in token && Array.isArray(token.items)) {
    slots.push(token.items);
  }
  if ('header' in token && Array.isArray(token.header)) {
    for (const cell of token.header) {
      slots.push(cell.tokens);
    }
  }
  if ('rows' in token && Array.isArray(token.rows)) {
    for (const row of token.rows) {
      for (const cell of row) {
        slots.push(cell.tokens);
      }
    }
  }
  return slots;
}

/**
 * Turn reference links and link definitions back into text, in place.
 *
 * The two halves need different treatment because `marked` keeps only one of them: a
 * definition line is lexed into a `def` token that the renderer has no method for, so it
 * silently disappears — the only way to show it again is to hand the block loop a
 * paragraph to render, which is what `processAllTokens` is for. A reference link *is*
 * rendered, as an ordinary `link` token whose `href` came from that definition; the
 * distinguisher is the source itself, since only an inline link carries `](`. Autolinks
 * and bare GFM URLs have no `](` either and are text in the subset, so they go the same
 * way — which is the right answer for the wrong-looking reason, and it is still the
 * subset's answer.
 */
function neutraliseReferences(tokens: Token[]): void {
  for (const [position, token] of tokens.entries()) {
    if (token.type === 'def') {
      const source = typeof token.raw === 'string' ? token.raw : '';
      tokens[position] = {
        type: 'paragraph',
        raw: source,
        text: source.trim(),
        tokens: [literalToken(source.trim())],
      };
      continue;
    }
    if (token.type === 'link' && !token.raw.includes('](')) {
      tokens[position] = literalToken(token.raw);
      continue;
    }
    for (const children of childSlots(token)) {
      neutraliseReferences(children);
    }
  }
}

const instance = new Marked();

instance.use({
  hooks: {
    processAllTokens(tokens) {
      neutraliseReferences(tokens);
      return tokens;
    },
  },
  renderer: {
    html(token) {
      const raw = typeof token.raw === 'string' ? token.raw : '';
      // A block-level HTML token came from its own line, so it keeps its paragraph
      // break as an escaped paragraph; an inline one stays inline, escaped.
      return token.block ? `<p>${escapeHtml(raw)}</p>` : escapeHtml(raw);
    },
    image(token) {
      // The source, not the element. `token.raw` is what was typed, which is what
      // `kestrel-md` shows — and it has to be that rather than a rebuild from `href`,
      // because a reference image (`![一张][ref]`) has already had its destination
      // resolved by the time this runs, and rebuilding would print a source line that
      // never existed.
      const raw = typeof token.raw === 'string' && token.raw.startsWith('![')
        ? token.raw
        : `![${token.text}](${token.href})`;
      return escapeHtml(raw);
    },
    checkbox(token) {
      // `marked` strips `[ ]` from the item's own text to make the control, so this is
      // where the marker comes back. The trailing space is what `kestrel-md` keeps.
      return token.checked ? '[x] ' : '[ ] ';
    },
    code(token) {
      const raw = typeof token.raw === 'string' ? token.raw : '';
      // Four spaces (or a tab) of indentation is an **indented code block** to CommonMark,
      // and this subset has no such construct: a fenced block is the code shape this product
      // uses. So the run goes back to being text — escaped, indentation dropped, and **not**
      // run through the inline pass, which is what keeps an over-indented fence from becoming
      // a paragraph of stray code spans. Same rule, same shape, as `kestrel-md`.
      // The discriminator is the source: a real fence starts with ```` ``` ```` or `~~~`
      // (after at most three columns), an indented block starts with four or with a tab.
      if (!/^ {4,}\S/.test(raw) && !/^\t/.test(raw)) {
        return false;
      }
      const text = raw
        .split('\n')
        .map((row) => row.trim())
        .join('\n')
        .trim();
      // The trailing newline is not decoration: marked's own `code` renderer ends with one and
      // the block separators between paragraphs come from it. Dropping it glues the next block
      // onto this one (`</p><p>`), which is a difference `kestrel-md` does not have.
      return `<p>${escapeHtml(text)}</p>\n`;
    },
  },
});

/**
 * Renders markdown synchronously — the shape both call sites need, because the SSR
 * template and the live stream both insert the result directly.
 */
export function renderMarkdown(source: string): string {
  try {
    return instance.parse(source, { async: false });
  } catch {
    // `marked` recurses too, and its limit is host-dependent: a line of 5,000 nested `>`
    // throws `RangeError: Maximum call stack size exceeded` (measured band: 3,000 survives,
    // 5,000 throws — docs/verification.md §46.6). Assistant text is model output, so that
    // input is reachable through prompt injection, and the answer cannot be a 500 in the SSR
    // path or a hole in the stream. The subset's own cap is 32 levels and it renders deeper
    // nesting as text, which is the same fallback: show the source.
    return `<p>${escapeHtml(source)}</p>\n`;
  }
}
