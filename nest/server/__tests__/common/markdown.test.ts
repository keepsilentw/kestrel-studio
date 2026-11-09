import { describe, expect, it } from 'vitest';
import { renderMarkdown } from '@/common/markdown';

/**
 * The content-injection policy of the Nest stack. `crates/md` escapes anything outside
 * its markdown subset and the Rust server renders with it; before this file, `marked`
 * with default options put model-authored HTML into the DOM instead, so the two online
 * implementations disagreed about what an assistant message is allowed to become.
 * The per-construct measurement that found it is docs/verification.md §36.2.
 */
describe('renderMarkdown — 原始 HTML 退回文本', () => {
  it('块级原始 HTML 不会成为元素，只显示源码', () => {
    const html = renderMarkdown('<img src=x onerror=alert(1)>');

    expect(html).toContain('&lt;img');
    expect(html).not.toContain('<img');
    // 块级那一条仍占一段，所以转义之后包在 <p> 里，与 kestrel-md 的段落形状一致。
    expect(html).toMatch(/^<p>/);
  });

  it('行内原始 HTML 也一样转义，不额外造段', () => {
    const html = renderMarkdown('前 <div class="x">块</div> 后');

    expect(html).toContain('&lt;div class="x"&gt;');
    expect(html).not.toContain('<div');
    // 逐字钉住输出，包括 marked 结尾那个换行：浏览器那份副本靠同一条断言对齐
    // （`nest/client/__tests__/markdown.test.ts`），少了这个 `\n` 两边就会互相打脸。
    expect(html).toBe('<p>前 &lt;div class="x"&gt;块&lt;/div&gt; 后</p>\n');
  });

  it('代码围栏里的标签只转义一次', () => {
    // 双重转义会把 &lt; 变成 &amp;lt;，那是「修好了但看得见」的错法：正文先转义再喂
    // marked 就会踩这个，所以这条断言是留着当反例的边界。
    const html = renderMarkdown('```html\n<div>code fence</div>\n```');

    expect(html).toContain('&lt;div&gt;code fence&lt;/div&gt;');
    expect(html).not.toContain('&amp;lt;');
  });

  it('子集内的 markdown 照旧渲染', () => {
    expect(renderMarkdown('**粗**')).toContain('<strong>粗</strong>');
    expect(renderMarkdown('- 甲\n- 乙')).toContain('<li>甲</li>');
    expect(renderMarkdown('## 标题')).toContain('<h2>标题</h2>');
    expect(renderMarkdown('[链接](https://example.com)')).toContain('href="https://example.com"');
    expect(renderMarkdown('| a | b |\n|---|---|\n| 1 | 2 |')).toContain('<table>');
  });
});
