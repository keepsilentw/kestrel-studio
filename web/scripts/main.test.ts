import { beforeEach, describe, expect, it, vi } from 'vitest';

/**
 * Drives the browser script through the DOM it actually manipulates.
 *
 * The fixture is hand-written and mirrors src/view/views/chat.hbs rather than
 * rendering that template, so a template change that drops an element this
 * script needs would NOT fail here — it fails at runtime instead. Keeping the
 * two in sync is on the template's side; see the note at the end of the file.
 */
const FIXTURE = `
  <header class="topbar">
    <nav class="menu-dropdown">
      <details>
        <summary>历史会话</summary>
        <ul class="conversation-list">
          <li class="conversation-item">
            <a class="conversation-link is-active" href="/?c=5">画一只红隼</a>
            <button type="button" class="conversation-delete" data-delete-conversation="5" aria-label="删除会话 画一只红隼" title="删除会话">删除</button>
          </li>
          <li class="conversation-item">
            <a class="conversation-link" href="/?c=6">另一个会话</a>
            <button type="button" class="conversation-delete" data-delete-conversation="6" aria-label="删除会话 另一个会话" title="删除会话">删除</button>
          </li>
        </ul>
      </details>
    </nav>
  </header>
  <aside class="sidebar">
    <button type="button" id="new-conversation">新会话</button>
    <ol id="turn-index"></ol>
  </aside>
  <main id="stream"></main>
  <div class="mode-selector" role="radiogroup" aria-label="模式">
    <button type="button" class="mode-option" data-mode="auto" role="radio" aria-checked="true">智能</button>
    <button type="button" class="mode-option" data-mode="chat" role="radio" aria-checked="false">对话</button>
    <button type="button" class="mode-option" data-mode="image" role="radio" aria-checked="false">图片</button>
    <button type="button" class="mode-option" data-mode="video" role="radio" aria-checked="false">视频</button>
  </div>
  <form id="composer-form">
    <textarea id="prompt"></textarea>
    <button type="submit" id="send">发送</button>
  </form>
  <dialog id="preview"><button type="button" id="preview-close"></button><img id="preview-image" alt="" /></dialog>
  <dialog id="download-dialog">
    <select id="download-format">
      <option value="image/png">PNG</option>
      <option value="image/jpeg">JPEG</option>
      <option value="image/webp">WebP</option>
    </select>
    <input type="range" id="download-quality" value="90" />
    <output id="download-quality-value"></output>
    <p id="download-hint"></p>
    <p id="download-estimate"></p>
    <button type="button" id="download-confirm">下载</button>
    <button type="button" id="download-cancel">取消</button>
  </dialog>
`;

const MODE_STORAGE_KEY = 'kestrel-studio:mode';

/** jsdom has no EventSource; this records what the script opened and lets a test fire events. */
let openedStreams: FakeEventSource[] = [];

class FakeEventSource {
  readonly url: string;
  closed = false;
  private readonly listeners = new Map<string, ((event: { data: string }) => void)[]>();

  constructor(url: string) {
    this.url = url;
    openedStreams.push(this);
  }

  addEventListener(type: string, listener: (event: { data: string }) => void): void {
    const existing = this.listeners.get(type) ?? [];
    existing.push(listener);
    this.listeners.set(type, existing);
  }

  close(): void {
    this.closed = true;
  }

  emit(type: string, payload: unknown): void {
    for (const listener of this.listeners.get(type) ?? []) {
      listener({ data: JSON.stringify(payload) });
    }
  }
}

async function boot(
  options: { conversationId?: string; storedMode?: string; canDelete?: boolean } = {},
): Promise<void> {
  vi.resetModules();
  document.body.innerHTML = FIXTURE;
  document.body.dataset.conversationId = options.conversationId ?? '5';
  // The server states this on <body>; only a normal account may delete.
  document.body.dataset.canDeleteConversations = options.canDelete === false ? 'false' : 'true';
  window.localStorage.clear();
  if (options.storedMode !== undefined) {
    window.localStorage.setItem(MODE_STORAGE_KEY, options.storedMode);
  }
  openedStreams = [];
  vi.stubGlobal('EventSource', FakeEventSource);
  await import('./main');
}

/** Answers the turn stream with the given frames, in one chunk, then closes it. */
function stubChatStream(frames: readonly string[]): void {
  const encoder = new TextEncoder();
  const body = new ReadableStream<Uint8Array>({
    start(controller): void {
      for (const frame of frames) {
        controller.enqueue(encoder.encode(frame));
      }
      controller.close();
    },
  });
  vi.stubGlobal('fetch', vi.fn(async () => ({ ok: true, status: 200, body })));
}

const frame = (event: string, data: unknown): string =>
  `event: ${event}\ndata: ${JSON.stringify(data)}\n\n`;

function element<T extends Element>(selector: string): T {
  const found = document.querySelector<T>(selector);
  if (found === null) {
    throw new Error(`fixture is missing ${selector}`);
  }
  return found;
}

function submit(prompt: string): void {
  const input = element<HTMLTextAreaElement>('#prompt');
  input.value = prompt;
  element<HTMLFormElement>('#composer-form').dispatchEvent(
    new Event('submit', { bubbles: true, cancelable: true }),
  );
}

function modeButton(mode: string): HTMLButtonElement {
  return element<HTMLButtonElement>(`.mode-option[data-mode="${mode}"]`);
}

/**
 * Answers the two calls the delete flow can make: the POST that removes the
 * conversation, and the list refresh that follows it.
 */
function stubDeleteFetch(options: { ok?: boolean; remaining?: { id: number; title: string }[] } = {}): void {
  const ok = options.ok ?? true;
  const remaining = options.remaining ?? [{ id: 5, title: '画一只红隼' }];
  vi.stubGlobal(
    'fetch',
    vi.fn(async (url: string) => {
      if (String(url) === '/api/conversations') {
        return { ok: true, status: 200, json: async () => remaining };
      }
      return { ok, status: ok ? 200 : 500, json: async () => ({ ok }) };
    }),
  );
}

function deleteButton(conversationId: number): HTMLButtonElement {
  return element<HTMLButtonElement>(`button[data-delete-conversation="${conversationId}"]`);
}

/** The second step of a delete, which only exists once a row has been asked. */
function confirmButton(): HTMLButtonElement {
  return element<HTMLButtonElement>('button[data-confirm-delete]');
}

function cancelButton(): HTMLButtonElement {
  return element<HTMLButtonElement>('button[data-cancel-delete]');
}

beforeEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe('启动', () => {
  it('对着真实结构的 DOM 启动不抛错', async () => {
    await expect(boot()).resolves.toBeUndefined();
  });

  it('已有会话时打开该会话的事件流', async () => {
    await boot({ conversationId: '42' });
    expect(openedStreams).toHaveLength(1);
    expect(openedStreams[0].url).toBe('/api/conversations/42/events');
  });

  it('新会话时不打开事件流', async () => {
    await boot({ conversationId: '' });
    expect(openedStreams).toEqual([]);
  });
});

describe('模式选择', () => {
  it('没有存储值时默认智能模式', async () => {
    await boot();
    expect(modeButton('auto').getAttribute('aria-checked')).toBe('true');
    expect(modeButton('image').getAttribute('aria-checked')).toBe('false');
  });

  it('启动时恢复上次选择的模式', async () => {
    await boot({ storedMode: 'video' });
    expect(modeButton('video').getAttribute('aria-checked')).toBe('true');
    expect(modeButton('auto').getAttribute('aria-checked')).toBe('false');
  });

  it('存储值非法时回退到智能模式', async () => {
    await boot({ storedMode: 'not-a-mode' });
    expect(modeButton('auto').getAttribute('aria-checked')).toBe('true');
  });

  it('点击后写入 localStorage 并转移激活态', async () => {
    await boot();
    modeButton('image').click();

    expect(window.localStorage.getItem(MODE_STORAGE_KEY)).toBe('image');
    expect(modeButton('image').getAttribute('aria-checked')).toBe('true');
    expect(modeButton('auto').getAttribute('aria-checked')).toBe('false');
    expect(modeButton('image').classList.contains('is-active')).toBe(true);
  });
});

describe('发送一轮', () => {
  it('POST 到 /api/chat，带上 prompt 与当前模式', async () => {
    await boot();
    stubChatStream([frame('connected', { conversationId: 5 }), frame('done', { messageId: 1 })]);

    submit('画一只红隼');
    await vi.waitFor(() => {
      expect(element<HTMLButtonElement>('#send').disabled).toBe(false);
    });

    const call = vi.mocked(fetch).mock.calls[0];
    expect(call[0]).toBe('/api/chat');
    expect(JSON.parse(String(call[1]?.body))).toEqual({
      prompt: '画一只红隼',
      mode: 'auto',
      conversationId: 5,
    });
  });

  it('空 prompt 不发请求', async () => {
    await boot();
    stubChatStream([]);
    submit('   ');
    await Promise.resolve();
    expect(vi.mocked(fetch)).not.toHaveBeenCalled();
  });

  it('提交后立刻出现用户轮次与助手占位', async () => {
    await boot();
    stubChatStream([frame('connected', { conversationId: 5 })]);

    submit('画一只红隼');
    await vi.waitFor(() => {
      expect(document.querySelector('.turn-user')).not.toBeNull();
    });

    expect(element('.turn-user').textContent).toContain('画一只红隼');
    expect(document.querySelector('.turn-assistant')).not.toBeNull();
  });

  it('reasoning 增量进入思考区，正文增量进入内容区', async () => {
    await boot();
    stubChatStream([
      frame('connected', { conversationId: 5 }),
      frame('reasoning', { delta: '先想构图' }),
      frame('text', { delta: '画好了' }),
      frame('done', { messageId: 1 }),
    ]);

    submit('画一只红隼');
    await vi.waitFor(() => {
      expect(element('.reasoning-body').textContent).toBe('先想构图');
    });
    expect(element('.turn-assistant .content').textContent).toContain('画好了');
  });

  it('模型没吐思考时，思考区被移除', async () => {
    await boot();
    stubChatStream([
      frame('connected', { conversationId: 5 }),
      frame('text', { delta: '直接回答' }),
      frame('done', { messageId: 1 }),
    ]);

    submit('你好');
    await vi.waitFor(() => {
      expect(element<HTMLButtonElement>('#send').disabled).toBe(false);
    });
    expect(document.querySelector('.reasoning')).toBeNull();
  });

  it('工具调用与结果合成一条可读的记录', async () => {
    await boot();
    stubChatStream([
      frame('connected', { conversationId: 5 }),
      frame('tool_call', { name: 'generate_image', arguments: {}, status: 'running' }),
      frame('tool_result', { name: 'generate_image', ok: true, summary: '1 image' }),
      frame('done', { messageId: 1 }),
    ]);

    submit('画一张图');
    await vi.waitFor(() => {
      expect(document.querySelector('.tool-call')).not.toBeNull();
    });
    expect(element('.tool-call').textContent).toContain('generate_image');
    expect(element('.tool-call').textContent).toContain('1 image');
  });

  it('asset 事件渲染成图片', async () => {
    await boot();
    stubChatStream([
      frame('connected', { conversationId: 5 }),
      frame('asset', { id: 3, kind: 'image', url: '/api/assets/3/download', mime: 'image/png', bytes: 1 }),
      frame('done', { messageId: 1 }),
    ]);

    submit('画一张图');
    await vi.waitFor(() => {
      expect(document.querySelector('img.asset-image')).not.toBeNull();
    });
    expect(element<HTMLImageElement>('img.asset-image').getAttribute('src')).toBe(
      '/api/assets/3/download',
    );
  });

  it('asset 事件按 kind 渲染成视频', async () => {
    await boot();
    stubChatStream([
      frame('connected', { conversationId: 5 }),
      frame('asset', { id: 4, kind: 'video', url: '/api/assets/4/download', mime: 'video/mp4', bytes: 1 }),
      frame('done', { messageId: 1 }),
    ]);

    submit('生成视频');
    await vi.waitFor(() => {
      expect(document.querySelector('video.asset-video')).not.toBeNull();
    });
  });

  it('error 事件显示为错误行', async () => {
    await boot();
    stubChatStream([frame('connected', { conversationId: 5 }), frame('error', { message: '出图失败' })]);

    submit('画一张图');
    await vi.waitFor(() => {
      expect(document.querySelector('.turn-error')).not.toBeNull();
    });
    expect(element('.turn-error').textContent).toBe('出图失败');
  });

  it('HTTP 非 2xx 时显示错误而不是静默', async () => {
    await boot();
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => ({ ok: false, status: 500, body: null })),
    );

    submit('画一张图');
    await vi.waitFor(() => {
      expect(document.querySelector('.turn-error')).not.toBeNull();
    });
    expect(element('.turn-error').textContent).toContain('500');
  });

  it('一轮结束后发送按钮与输入框恢复可用', async () => {
    await boot();
    stubChatStream([frame('connected', { conversationId: 5 }), frame('done', { messageId: 1 })]);

    submit('你好');
    await vi.waitFor(() => {
      expect(element<HTMLButtonElement>('#send').disabled).toBe(false);
    });
    expect(element<HTMLTextAreaElement>('#prompt').disabled).toBe(false);
  });
});

describe('会话事件流', () => {
  it('message_added 追加一条后台完成的轮次', async () => {
    // A video render finishes minutes after its turn; the result arrives here,
    // not on the turn stream.
    await boot({ conversationId: '5' });
    const stream = openedStreams[0];

    stream.emit('message_added', {
      type: 'message_added',
      message: {
        id: 77,
        role: 'assistant',
        content: '视频好了',
        reasoning: null,
        toolCalls: null,
        mode: 'video',
        createdAt: 1,
        assets: [
          { id: 9, kind: 'video', url: '/api/assets/9/download', mime: 'video/mp4', bytes: 2 },
        ],
      },
    });

    await vi.waitFor(() => {
      expect(document.querySelector('[data-message-id="77"]')).not.toBeNull();
    });
    expect(element('[data-message-id="77"]').textContent).toContain('视频好了');
    expect(document.querySelector('[data-message-id="77"] video.asset-video')).not.toBeNull();
  });

  it('task_updated 排队中时显示进行中的任务', async () => {
    await boot({ conversationId: '5' });
    openedStreams[0].emit('task_updated', {
      taskId: 12,
      kind: 'video',
      status: 'queued',
    });

    await vi.waitFor(() => {
      expect(document.querySelector('.task-pending[data-task-id="12"]')).not.toBeNull();
    });
    expect(element('.task-pending').textContent).toContain('排队中');
  });

  it('任务成功后被移除', async () => {
    await boot({ conversationId: '5' });
    const stream = openedStreams[0];

    stream.emit('task_updated', { taskId: 12, kind: 'video', status: 'running' });
    await vi.waitFor(() => {
      expect(document.querySelector('.task-pending[data-task-id="12"]')).not.toBeNull();
    });

    stream.emit('task_updated', { taskId: 12, kind: 'video', status: 'succeeded' });
    await vi.waitFor(() => {
      expect(document.querySelector('.task-pending[data-task-id="12"]')).toBeNull();
    });
  });

  it('任务失败后同样被移除', async () => {
    await boot({ conversationId: '5' });
    const stream = openedStreams[0];

    stream.emit('task_updated', { taskId: 13, kind: 'video', status: 'queued' });
    await vi.waitFor(() => {
      expect(document.querySelector('.task-pending[data-task-id="13"]')).not.toBeNull();
    });

    stream.emit('task_updated', { taskId: 13, kind: 'video', status: 'failed' });
    await vi.waitFor(() => {
      expect(document.querySelector('.task-pending[data-task-id="13"]')).toBeNull();
    });
  });
});

describe('历史会话删除', () => {
  it('第一次点击只是问一次，不发请求', async () => {
    await boot();
    stubDeleteFetch();

    deleteButton(6).click();
    await Promise.resolve();

    expect(vi.mocked(fetch)).not.toHaveBeenCalled();
    expect(element('.conversation-confirm').textContent).toContain('删除？');
    expect(confirmButton().dataset.confirmDelete).toBe('6');
    // 原来那个「删除」被收起，同一行不会有两个入口
    expect(deleteButton(6).hidden).toBe(true);
  });

  it('点取消回到原样，也不发请求', async () => {
    await boot();
    stubDeleteFetch();

    deleteButton(6).click();
    cancelButton().click();
    await Promise.resolve();

    expect(vi.mocked(fetch)).not.toHaveBeenCalled();
    expect(document.querySelector('.conversation-confirm')).toBeNull();
    expect(deleteButton(6).hidden).toBe(false);
  });

  it('点确认才发请求，随后刷新列表', async () => {
    await boot();
    stubDeleteFetch();

    deleteButton(6).click();
    confirmButton().click();
    await vi.waitFor(() => {
      expect(document.querySelector('button[data-delete-conversation="6"]')).toBeNull();
    });

    const requested = vi.mocked(fetch).mock.calls.map((call) => String(call[0]));
    expect(requested).toContain('/api/conversations/6/delete');
    expect(requested).toContain('/api/conversations');
    // 删的不是当前会话，页面不该被重置
    expect(document.body.dataset.conversationId).toBe('5');
  });

  it('删掉当前会话后回到新会话状态，并关掉事件流', async () => {
    await boot({ conversationId: '5' });
    stubDeleteFetch({ remaining: [{ id: 6, title: '另一个会话' }] });
    const stream = openedStreams[0];

    deleteButton(5).click();
    confirmButton().click();
    await vi.waitFor(() => {
      expect(stream.closed).toBe(true);
    });

    expect(document.body.dataset.conversationId).toBeUndefined();
    expect(element('#stream').children).toHaveLength(0);
    expect(window.location.search).toBe('');
    expect(document.querySelector('button[data-delete-conversation="6"]')).not.toBeNull();
  });

  it('服务端失败时在列表里说明，不动当前会话', async () => {
    await boot();
    stubDeleteFetch({ ok: false });

    deleteButton(6).click();
    confirmButton().click();
    await vi.waitFor(() => {
      expect(document.querySelector('.conversation-error')).not.toBeNull();
    });

    expect(element('.conversation-error').textContent).toContain('500');
    expect(document.querySelector('button[data-delete-conversation="6"]')).not.toBeNull();
  });

  it('id 解析不出来的按钮确认后也不触发请求', async () => {
    await boot();
    stubDeleteFetch();
    const item = document.createElement('li');
    item.className = 'conversation-item';
    const stray = document.createElement('button');
    stray.type = 'button';
    stray.className = 'conversation-delete';
    stray.dataset.deleteConversation = 'abc';
    item.append(stray);
    element('.conversation-list').append(item);

    stray.click();
    confirmButton().click();
    await Promise.resolve();

    expect(vi.mocked(fetch)).not.toHaveBeenCalled();
  });

  it('不在列表项里的按钮什么都不做', async () => {
    // The row is what holds the state the second step needs, so a button without
    // one is inert rather than half-working.
    await boot();
    stubDeleteFetch();
    const stray = document.createElement('button');
    stray.dataset.deleteConversation = '6';
    element('.conversation-list').append(stray);

    stray.click();
    await Promise.resolve();

    expect(document.querySelector('.conversation-confirm')).toBeNull();
    expect(vi.mocked(fetch)).not.toHaveBeenCalled();
  });

  it('超管那一侧重建列表时不带删除控件', async () => {
    // The endpoint refuses the role too; this is the affordance side of the rule.
    await boot({ canDelete: false });
    stubDeleteFetch();

    element<HTMLButtonElement>('#new-conversation').click();

    // Waiting on the absence itself: the fixture starts with controls, so any
    // earlier signal would already be true.
    await vi.waitFor(() => {
      expect(document.querySelector('button[data-delete-conversation]')).toBeNull();
    });
    expect(document.querySelector('.conversation-link')).not.toBeNull();
  });
});

/**
 * The fixture above mirrors src/view/views/chat.hbs rather than rendering it:
 * the client script cannot import server files without the `@` alias and Node
 * globals leaking into browser code. Template drift is therefore caught on the
 * server side instead — src/view/view.controller.test.ts renders the real
 * template and asserts every id this file's fixture depends on is present.
 * Change a template id and that suite fails; this one keeps testing behaviour.
 */
