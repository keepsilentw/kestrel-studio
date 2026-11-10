import { Marked, type Token, type Tokens } from 'marked';

/**
 * The browser half of `nest/server/src/common/markdown.ts` — read that file first.
 *
 * Same policy, same hook, same reason: assistant text is model output, and the two
 * stacks of this repository have to agree about whether raw HTML in it reaches the DOM.
 * The live-stream path (`text` events, `main.ts`) and the reload path (`buildUserTurn`
 * on stored content) both go through here, so a turn cannot look different before and
 * after a refresh — which is the same property the SSR/`seq` replay work kept.
 *
 * Kept as a second copy rather than a shared package because there is no shared JS
 * package in this monorepo, and adding one means touching `pnpm-workspace.yaml` and
 * the lockfile. If you change one, change the other in the same commit.
 */

/**
 * Byte-identical to `escape()` in `crates/md` — the same five characters in the same
 * spelling, which is what makes the two stacks' text output equal rather than equivalent.
 */
function escapeHtml(value: string): string {
  return value
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
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

/** A token that carries nothing but the source it was written from. */
function literalToken(raw: string): Tokens.Text {
  return { type: 'text', raw, text: raw };
}

/**
 * Reference links and link definitions back into text — the subset's answer, and the
 * mirror of `nest/server/src/common/markdown.ts`. The reason each half has to be handled
 * where it is (a definition through `processAllTokens`, a reference through the token) is
 * written up there.
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
      return token.block ? `<p>${escapeHtml(raw)}</p>` : escapeHtml(raw);
    },
    image(token) {
      // The source, not the element — the same reason as the server twin: a reference
      // image has already had its destination resolved here, so rebuilding from `href`
      // would print a line nobody typed.
      const raw = typeof token.raw === 'string' && token.raw.startsWith('![')
        ? token.raw
        : `![${token.text}](${token.href})`;
      return escapeHtml(raw);
    },
    checkbox(token) {
      return token.checked ? '[x] ' : '[ ] ';
    },
    code(token) {
      const raw = typeof token.raw === 'string' ? token.raw : '';
      // Four columns or a tab is an indented code block to CommonMark, and this subset has no
      // such construct: the run goes back to being escaped text, indentation dropped and the
      // inline pass not run over it. Same cap and same shape as `kestrel-md` and as the server
      // twin — read `nest/server/src/common/markdown.ts` for the reasoning.
      if (!/^ {4,}\S/.test(raw) && !/^\t/.test(raw)) {
        return false;
      }
      const text = raw
        .split('\n')
        .map((row) => row.trim())
        .join('\n')
        .trim();
      // The trailing newline is marked's own block separator; without it the next block glues
      // onto this one as `</p><p>`.
      return `<p>${escapeHtml(text)}</p>\n`;
    },
  },
});

export function renderMarkdown(source: string): string {
  try {
    return instance.parse(source, { async: false });
  } catch {
    // Same fallback as the server twin, same reason: `marked` recurses and its limit is
    // host-dependent (5,000 nested `>` throws RangeError, 3,000 does not). A model can be
    // prompt-injected into producing that, and the live-stream path re-renders the whole
    // answer on every delta — a throw here would stop the turn mid-sentence.
    return `<p>${escapeHtml(source)}</p>\n`;
  }
}
