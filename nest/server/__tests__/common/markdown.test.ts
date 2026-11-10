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

    // 五个字符一套转义，与 `crates/md` 的 `escape()` 同一拼法（引号也算）。
    expect(html).toContain('&lt;div class=&quot;x&quot;&gt;');
    expect(html).not.toContain('<div');
    // 逐字钉住输出，包括 marked 结尾那个换行：浏览器那份副本靠同一条断言对齐
    // （`nest/client/__tests__/markdown.test.ts`），少了这个 `\n` 两边就会互相打脸。
    expect(html).toBe('<p>前 &lt;div class=&quot;x&quot;&gt;块&lt;/div&gt; 后</p>\n');
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

/**
 * The rest of the口径: `crates/md` is the contract, and these are the constructs that used
 * to render on one side only. Each expected string below was taken from the same run of
 * both engines over the same samples (docs/verification.md §46), not written by hand — the
 * ones in this file are byte-for-byte what `markdown()` in Rust returns.
 */
describe('renderMarkdown — 子集之外退回文字', () => {
  it('四格缩进不是代码块：这个子集里没有缩进代码这一式', () => {
    expect(renderMarkdown('段落\n\n    四格缩进的一行\n\n结束')).toBe(
      '<p>段落</p>\n<p>四格缩进的一行</p>\n<p>结束</p>\n',
    );
    expect(renderMarkdown('    - 甲\n    - 乙')).toBe('<p>- 甲\n- 乙</p>\n');
    // 围栏缩进四格也照文字处理，不会变成一段乱码的 code span
    expect(renderMarkdown('    ```js\n    let x = 1;\n    ```')).toBe(
      '<p>```js\nlet x = 1;\n```</p>\n',
    );
  });

  it('图片不是元素：资产来自工具管线，不由模型写的 markdown 给', () => {
    expect(renderMarkdown('![红隼](https://example.test/a.png)')).toBe(
      '<p>![红隼](https://example.test/a.png)</p>\n',
    );
    // 引用式图片（`![alt][ref]`）退回的是**写下的那串**，不是解析后的目标。
    expect(renderMarkdown('图 ![一张][ref]\n\n[ref]: https://example.test/a.png')).toContain(
      '<p>图 ![一张][ref]</p>',
    );
  });

  it('任务列表没有表单控件，方括号留着', () => {
    expect(renderMarkdown('- [ ] 甲\n- [x] 乙')).toBe(
      '<ul>\n<li>[ ] 甲</li>\n<li>[x] 乙</li>\n</ul>\n',
    );
  });

  it('引用链接与它的定义行都留在原地；脚注走的是同一条路', () => {
    expect(renderMarkdown('有脚注[^1]。\n\n[^1]: 说明一行')).toBe(
      '<p>有脚注[^1]。</p>\n<p>[^1]: 说明一行</p>\n',
    );
    expect(renderMarkdown('见 [文档][ref]。\n\n[ref]: https://example.test/x?a=1&b=2')).toBe(
      '<p>见 [文档][ref]。</p>\n<p>[ref]: https://example.test/x?a=1&amp;b=2</p>\n',
    );
    // 行内链接仍然照渲染——退回文字的是引用式那两种写法。
    expect(renderMarkdown('[红隼](https://example.test?a=1&b=2)')).toBe(
      '<p><a href="https://example.test?a=1&amp;b=2">红隼</a></p>\n',
    );
  });

  it('裸 URL 与尖括号自动链接也只是文字', () => {
    expect(renderMarkdown('访问 https://example.test/a?x=1&y=2 看看')).toBe(
      '<p>访问 https://example.test/a?x=1&amp;y=2 看看</p>\n',
    );
    expect(renderMarkdown('<https://example.test/a>')).toBe('<p>&lt;https://example.test/a&gt;</p>\n');
  });
});

/**
 * The five block constructs that moved **into** the subset. These are marked's own shapes
 * — they were already right on this side, and the Rust renderer is the one that changed.
 */
describe('renderMarkdown — 块级构造（Rust 侧补齐的那五条）', () => {
  it('引用块、分隔线、setext 标题、嵌套列表、~~~ 围栏', () => {
    expect(renderMarkdown('> 引用一行')).toBe('<blockquote>\n<p>引用一行</p>\n</blockquote>\n');
    expect(renderMarkdown('---')).toBe('<hr>\n');
    expect(renderMarkdown('标题\n===')).toBe('<h1>标题</h1>\n');
    expect(renderMarkdown('标题\n---')).toBe('<h2>标题</h2>\n');
    expect(renderMarkdown('- 甲\n  - 乙\n- 丙')).toBe(
      '<ul>\n<li>甲<ul>\n<li>乙</li>\n</ul>\n</li>\n<li>丙</li>\n</ul>\n',
    );
    expect(renderMarkdown('~~~js\nconsole.log(1)\n~~~')).toBe(
      '<pre><code class="language-js">console.log(1)\n</code></pre>\n',
    );
    expect(renderMarkdown('文本\n- - -')).toBe('<p>文本</p>\n<hr>\n');
  });
});

/**
 * 递归渲染器有自己的深度上限，而且它吃的是模型输出。实测：一行 5,000 个 `>` 让 marked 抛
 * `RangeError: Maximum call stack size exceeded`（3,000 层不抛）。抛出来不是「这一条消息渲染
 * 失败」那么简单——SSR 那一路会把整个请求打回 500，所以出口把它接住，退成整段文字。
 * Rust 那一侧不抛而是**栈溢出终止进程**（会带走所有在连的 SSE），所以那边是一条 32 层的硬上限。
 */
describe('renderMarkdown — 深度炸不了这一屏', () => {
  it('五千层嵌套引用不抛，退成文字', () => {
    const source = `${'>'.repeat(5_000)} 深`;

    expect(() => renderMarkdown(source)).not.toThrow();
    const html = renderMarkdown(source);

    expect(html.startsWith('<p>&gt;')).toBe(true);
    expect(html).toContain('深');
    expect(html).not.toContain('<blockquote><blockquote><blockquote><blockquote><blockquote><blockquote>');
  });

  it('三十层以内照旧正常渲染', () => {
    const html = renderMarkdown(`${'>'.repeat(30)} 深`);

    expect(html.match(/<blockquote>/g)).toHaveLength(30);
  });
});

describe('renderMarkdown — 与 crates/md 逐字节同形', () => {
  it('一份混合文档，两套引擎输出同一串', () => {
    const input = [
      '![隼](https://example.test/a.png)',
      '',
      '> 引用一行',
      '',
      '- 甲',
      '  - 乙',
      '',
      '文本 **粗** [文档][ref]',
      '',
      '[ref]: https://example.test/x?a=1&b=2',
    ].join('\n');
    const expected =
      '<p>![隼](https://example.test/a.png)</p>\n' +
      '<blockquote>\n<p>引用一行</p>\n</blockquote>\n' +
      '<ul>\n<li>甲<ul>\n<li>乙</li>\n</ul>\n</li>\n</ul>\n' +
      '<p>文本 <strong>粗</strong> [文档][ref]</p>\n' +
      '<p>[ref]: https://example.test/x?a=1&amp;b=2</p>\n';

    expect(renderMarkdown(input)).toBe(expected);
  });
});
