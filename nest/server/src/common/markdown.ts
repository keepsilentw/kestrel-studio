import { Marked } from 'marked';

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

/** Escapes the three characters that make text into markup. */
function escapeHtml(value: string): string {
  return value.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

const instance = new Marked();

instance.use({
  renderer: {
    html(token) {
      const raw = typeof token.raw === 'string' ? token.raw : '';
      // A block-level HTML token came from its own line, so it keeps its paragraph
      // break as an escaped paragraph; an inline one stays inline, escaped.
      return token.block ? `<p>${escapeHtml(raw)}</p>` : escapeHtml(raw);
    },
  },
});

/**
 * Renders markdown synchronously — the shape both call sites need, because the SSR
 * template and the live stream both insert the result directly.
 */
export function renderMarkdown(source: string): string {
  return instance.parse(source, { async: false });
}
