import { Marked } from 'marked';

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

function escapeHtml(value: string): string {
  return value.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

const instance = new Marked();

instance.use({
  renderer: {
    html(token) {
      const raw = typeof token.raw === 'string' ? token.raw : '';
      return token.block ? `<p>${escapeHtml(raw)}</p>` : escapeHtml(raw);
    },
  },
});

export function renderMarkdown(source: string): string {
  return instance.parse(source, { async: false });
}
