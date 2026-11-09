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
      '<p>前 &lt;div class="x"&gt;块&lt;/div&gt; 后</p>\n<ul>\n<li>甲</li>\n</ul>\n<pre><code class="language-html">&lt;i&gt;y&lt;/i&gt;\n</code></pre>\n';

    expect(renderMarkdown(input)).toBe(expected);
  });
});
