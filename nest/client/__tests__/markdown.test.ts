import { describe, expect, it } from 'vitest';
import { renderMarkdown } from '../scripts/markdown';

/**
 * The browser half of the content-injection policy. Same rules as
 * `nest/server/__tests__/common/markdown.test.ts` — the two files are the guard that
 * keeps the live-stream path and the reload path from disagreeing with the server, and
 * with the Rust client, about what an assistant message may become in the DOM.
 */
function mount(html: string): HTMLDivElement {
  const host = document.createElement('div');
  host.innerHTML = html;
  return host;
}

describe('renderMarkdown（浏览器侧）— 原始 HTML 不进 DOM', () => {
  it('onerror 探针挂不成元素，源码作为文字可见', () => {
    const host = mount(renderMarkdown('<img src=x onerror=alert(1)>'));

    expect(host.querySelector('img')).toBeNull();
    expect(host.textContent).toContain('<img src=x onerror=alert(1)>');
  });

  it('行内 HTML 同样只是文字', () => {
    const host = mount(renderMarkdown('列表项 <b>粗</b>'));

    expect(host.querySelector('b')).toBeNull();
    expect(host.textContent).toContain('<b>粗</b>');
  });

  it('markdown 照常渲染成结构', () => {
    const host = mount(renderMarkdown('**粗** 与 [链接](https://example.com)'));

    expect(host.querySelector('strong')?.textContent).toBe('粗');
    expect(host.querySelector('a')?.getAttribute('href')).toBe('https://example.com');
  });

  it('与服务器那一份同形（同一条输入，同一串输出）', () => {
    // 两份实现是手抄同步的（没有共享 JS 包），所以这条断言就是那根同步线：改了这里
    // 就要改 nest/server/src/common/markdown.ts，否则两套渲染会静默分叉。
    const input = '前 <div class="x">块</div> 后\n\n- 甲\n\n```html\n<i>y</i>\n```';
    const expected =
      '<p>前 &lt;div class=&quot;x&quot;&gt;块&lt;/div&gt; 后</p>\n' +
      '<ul>\n<li>甲</li>\n</ul>\n' +
      '<pre><code class="language-html">&lt;i&gt;y&lt;/i&gt;\n</code></pre>\n';

    expect(renderMarkdown(input)).toBe(expected);
  });
});

/**
 * 子集之外的东西只能成为文字（`crates/md` 是唯一口径，docs/roadmap.md §4.1.1）。
 * 浏览器这侧要看的是**元素有没有出现**，因为流式渲染走的是 `set_inner_html` /
 * `innerHTML`：源码里看起来对不算对，DOM 里没有 `<img>`、没有 `<input>` 才算对。
 */
describe('renderMarkdown（浏览器侧）— 子集之外不成元素', () => {
  it('图片不生成元素', () => {
    const host = mount(renderMarkdown('![红隼](https://example.test/a.png)'));

    expect(host.querySelector('img')).toBeNull();
    // marked 在每个块后面留一个换行，所以这里比的是去掉它的文字本身。
    expect(host.textContent?.trim()).toBe('![红隼](https://example.test/a.png)');
  });

  it('任务列表不生成表单控件', () => {
    const host = mount(renderMarkdown('- [ ] 甲\n- [x] 乙'));

    expect(host.querySelector('input')).toBeNull();
    expect(host.textContent).toContain('[ ] 甲');
    expect(host.textContent).toContain('[x] 乙');
  });

  it('引用式链接不生成 a，定义行仍然看得见', () => {
    const host = mount(renderMarkdown('见 [文档][ref]。\n\n[ref]: https://example.test/x'));

    expect(host.querySelector('a')).toBeNull();
    expect(host.textContent).toContain('见 [文档][ref]。');
    expect(host.textContent).toContain('[ref]: https://example.test/x');
  });

  it('裸 URL 与尖括号自动链接也只是文字', () => {
    const bare = mount(renderMarkdown('访问 https://example.test/a?x=1&y=2 看看'));

    expect(bare.querySelector('a')).toBeNull();
    expect(bare.textContent?.trim()).toBe('访问 https://example.test/a?x=1&y=2 看看');
    expect(mount(renderMarkdown('<https://example.test/a>')).querySelector('a')).toBeNull();
  });
});

describe('renderMarkdown（浏览器侧）— 缩进不是代码块', () => {
  it('四格缩进的一段只是文字', () => {
    const host = mount(renderMarkdown('段落\n\n    四格缩进的一行'));

    expect(host.querySelector('pre')).toBeNull();
    expect(host.textContent).toContain('四格缩进的一行');
  });

  it('与服务器那一份同形：同一条缩进输入', () => {
    const input = '段落\n\n    四格缩进的一行\n\n结束';

    expect(renderMarkdown(input)).toBe('<p>段落</p>\n<p>四格缩进的一行</p>\n<p>结束</p>\n');
  });
});

/** 块级那五条本来就在这一侧是对的，Rust 侧补齐后两边同形（§46）。 */
describe('renderMarkdown（浏览器侧）— 块级构造成为结构', () => {
  it('引用块 / 分隔线 / setext / 嵌套列表 / ~~~ 围栏', () => {
    expect(mount(renderMarkdown('> 引用一行')).querySelector('blockquote p')?.textContent).toBe(
      '引用一行',
    );
    expect(mount(renderMarkdown('---')).querySelectorAll('hr')).toHaveLength(1);
    expect(mount(renderMarkdown('标题\n===')).querySelector('h1')?.textContent).toBe('标题');
    expect(mount(renderMarkdown('- 甲\n  - 乙')).querySelector('ul ul li')?.textContent).toBe('乙');
    const fenced = mount(renderMarkdown('~~~js\nconsole.log(1)\n~~~'));

    expect(fenced.querySelector('pre code')?.getAttribute('class')).toBe('language-js');
    expect(fenced.textContent).toContain('console.log(1)');
  });
});

/** 深度这条在浏览器侧同样要过：流式那一路每个 delta 都重渲染一次整段回答。 */
describe('renderMarkdown（浏览器侧）— 深度炸不了这一屏', () => {
  it('五千层嵌套引用不抛，退成文字', () => {
    const source = `${'>'.repeat(5_000)} 深`;

    expect(() => renderMarkdown(source)).not.toThrow();
    const host = mount(renderMarkdown(source));

    expect(host.querySelectorAll('blockquote')).toHaveLength(0);
    expect(host.textContent).toContain('深');
  });

  it('三十层以内照旧套起来', () => {
    const host = mount(renderMarkdown(`${'>'.repeat(30)} 深`));

    expect(host.querySelectorAll('blockquote')).toHaveLength(30);
  });
});

describe('renderMarkdown（浏览器侧）— 与 crates/md 逐字节同形', () => {
  it('一份混合文档，输出与服务器那一份、与 Rust 那一个渲染器同一串', () => {
    // 同步线第二根：同一条输入在三处（本文件、`nest/server/__tests__/common/markdown.test.ts`、
    // `rust/server/src/view/tests.rs`）钉同一段输出。改任何一侧都会在这里红。
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
