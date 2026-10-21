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
  <dialog id="conversation-delete-dialog" aria-labelledby="conversation-delete-title">
    <h2 id="conversation-delete-title">从历史里删除这条会话？</h2>
    <p id="conversation-delete-target"></p>
    <button type="button" data-delete-choice="cancel">取消</button>
    <button type="button" data-delete-choice="confirm">删除</button>
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
  options: {
    conversationId?: string;
    startSeq?: string;
    storedMode?: string;
    canDelete?: boolean;
  } = {},
): Promise<void> {
  vi.resetModules();
  document.body.innerHTML = FIXTURE;
  document.body.dataset.conversationId = options.conversationId ?? '5';
  // The server stamps where its first paint ends in the event journal; the
  // script passes it as ?from= on the stream's first connection.
  document.body.dataset.startSeq = options.startSeq ?? '55';
  // The server states this on <body>; only a normal account may delete.
  document.body.dataset.canDeleteConversations = options.canDelete === false ? 'false' : 'true';
  window.localStorage.clear();
  if (options.storedMode !== undefined) {
    window.localStorage.setItem(MODE_STORAGE_KEY, options.storedMode);
  }
  openedStreams = [];
  vi.stubGlobal('EventSource', FakeEventSource);
  await import('../../web/scripts/main');
}

interface StoredMessageFixture {
  id: number;
  role: 'user' | 'assistant';
  content: string;
  mode?: string | null;
  reasoning?: string | null;
  toolCalls?: { name: string; summary: string }[] | null;
  assets?: { id: number; kind: string; url: string; mime: string; bytes: number }[];
}

function storedMessage(overrides: StoredMessageFixture): Record<string, unknown> {
  return {
    id: overrides.id,
    role: overrides.role,
    content: overrides.content,
    reasoning: overrides.reasoning ?? null,
    toolCalls: overrides.toolCalls ?? null,
    mode: overrides.mode ?? null,
    createdAt: 1,
    assets: overrides.assets ?? [],
  };
}

/**
 * Answers the JSON endpoints the script calls. The stream is not fetch-based
 * (it is an EventSource, handled by FakeEventSource above), so nothing here
 * returns SSE bodies: submitting answers 202 and the test then feeds the
 * stream by hand — the same split the server now enforces.
 */
function stubApi(
  options: {
    create?: { conversationId: number };
    turns?: { status?: number; body?: unknown };
    list?: { id: number; title: string }[];
  } = {},
): void {
  const turnStatus = options.turns?.status ?? 202;
  const turnBody = options.turns?.body ?? { conversationId: 5, messageId: 9 };
  const create = options.create;
  vi.stubGlobal(
    'fetch',
    vi.fn(async (url: string, init?: { method?: string }) => {
      const path = String(url);
      if (path === '/api/v1/conversations') {
        if ((init?.method ?? 'GET') === 'POST' && create !== undefined) {
          return { ok: true, status: 201, json: async () => ({ conversationId: create.conversationId }) };
        }
        return { ok: true, status: 200, json: async () => options.list ?? [] };
      }
      if (/^\/api\/v1\/conversations\/\d+\/turns$/.test(path)) {
        return {
          ok: turnStatus === 202,
          status: turnStatus,
          json: async () => turnBody,
        };
      }
      if (/^\/api\/v1\/conversations\/\d+\/delete$/.test(path)) {
        return { ok: true, status: 200, json: async () => ({ ok: true }) };
      }
      return { ok: true, status: 200, json: async () => [] };
    }),
  );
}

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
      if (String(url) === '/api/v1/conversations') {
        return { ok: true, status: 200, json: async () => remaining };
      }
      return { ok, status: ok ? 200 : 500, json: async () => ({ ok }) };
    }),
  );
}

function deleteButton(conversationId: number): HTMLButtonElement {
  return element<HTMLButtonElement>(`button[data-delete-conversation="${conversationId}"]`);
}

/** The modal that asks before a conversation is hidden from the history. */
function deleteDialog(): HTMLDialogElement {
  return element<HTMLDialogElement>('#conversation-delete-dialog');
}

/** One of its two answers, addressed by the choice it stands for. */
function deleteChoice(choice: 'confirm' | 'cancel'): HTMLButtonElement {
  const selector = `#conversation-delete-dialog button[data-delete-choice="${choice}"]`;
  return element<HTMLButtonElement>(selector);
}

beforeEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe('启动', () => {
  it('对着真实结构的 DOM 启动不抛错', async () => {
    await expect(boot()).resolves.toBeUndefined();
  });

  it('已有会话时打开该会话的事件流，游标接在首屏后面', async () => {
    await boot({ conversationId: '42', startSeq: '55' });
    expect(openedStreams).toHaveLength(1);
    expect(openedStreams[0].url).toBe('/api/v1/conversations/42/stream?from=55');
  });

  it('首屏没带游标时不伪造一个：由服务端定追到哪', async () => {
    await boot({ conversationId: '42', startSeq: '' });
    expect(openedStreams[0].url).toBe('/api/v1/conversations/42/stream');
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

describe('发送一轮 — POST 只提交，画面由事件流绘制', () => {
  it('POST 到 /api/v1/conversations/:id/turns，带上 prompt 与当前模式', async () => {
    await boot();
    stubApi({ turns: { status: 202, body: { conversationId: 5, messageId: 9 } } });

    submit('画一只红隼');
    await vi.waitFor(() => {
      expect(vi.mocked(fetch).mock.calls.length).toBeGreaterThan(0);
    });

    const call = vi.mocked(fetch).mock.calls[0];
    expect(call[0]).toBe('/api/v1/conversations/5/turns');
    expect(call[1]?.method).toBe('POST');
    expect(JSON.parse(String(call[1]?.body))).toEqual({ prompt: '画一只红隼', mode: 'auto' });

    // 回答不再挂在这条 POST 上：提交之后没有响应流可读，
    // 收尾由会话事件流说了算。
    openedStreams[0].emit('turn_done', { type: 'turn_done', messageId: 9 });
    await vi.waitFor(() => {
      expect(element<HTMLButtonElement>('#send').disabled).toBe(false);
    });
  });

  it('空 prompt 不发请求', async () => {
    await boot();
    stubApi();
    submit('   ');
    await Promise.resolve();
    expect(vi.mocked(fetch)).not.toHaveBeenCalled();
  });

  it('轮次不本地拼装：用户回声与助手占位都由流里画', async () => {
    await boot();
    stubApi({ turns: { status: 202, body: { conversationId: 5, messageId: 9 } } });

    submit('画一只红隼');
    await vi.waitFor(() => {
      expect(vi.mocked(fetch).mock.calls.length).toBeGreaterThan(0);
    });
    // 202 已经回来，事件还没到 —— DOM 一动不动。
    expect(document.querySelector('.turn-user')).toBeNull();
    expect(document.querySelector('.turn-assistant')).toBeNull();

    const source = openedStreams[0];
    source.emit('user_message', {
      type: 'user_message',
      message: storedMessage({ id: 8, role: 'user', content: '画一只红隼', mode: 'auto' }),
    });
    source.emit('turn_started', { type: 'turn_started', messageId: 9, mode: 'auto' });

    await vi.waitFor(() => {
      expect(document.querySelector('.turn-assistant')).not.toBeNull();
    });
    expect(element('.turn-user').textContent).toContain('画一只红隼');
    expect(document.querySelector('.turn-assistant[data-message-id="9"]')).not.toBeNull();
  });

  it('reasoning 增量进思考区、text 增量进内容区，都落在自己的 messageId 上', async () => {
    await boot({ conversationId: '5' });
    const source = openedStreams[0];

    source.emit('turn_started', { type: 'turn_started', messageId: 9, mode: 'image' });
    source.emit('reasoning', { type: 'reasoning', messageId: 9, delta: '先想构图' });
    source.emit('text', { type: 'text', messageId: 9, delta: '画好了' });

    await vi.waitFor(() => {
      expect(element('[data-message-id="9"] .reasoning-body').textContent).toBe('先想构图');
    });
    expect(element('[data-message-id="9"] .content').textContent).toContain('画好了');
  });

  it('模型没吐思考时，思考区被移除', async () => {
    await boot({ conversationId: '5' });
    const source = openedStreams[0];

    source.emit('turn_started', { type: 'turn_started', messageId: 9, mode: 'chat' });
    source.emit('text', { type: 'text', messageId: 9, delta: '直接回答' });
    source.emit('turn_done', { type: 'turn_done', messageId: 9 });

    await vi.waitFor(() => {
      expect(document.querySelector('[data-message-id="9"] .content')).not.toBeNull();
    });
    expect(document.querySelector('[data-message-id="9"] .reasoning')).toBeNull();
  });

  it('工具调用与结果合成一条可读的记录', async () => {
    await boot({ conversationId: '5' });
    const source = openedStreams[0];

    source.emit('turn_started', { type: 'turn_started', messageId: 9, mode: 'image' });
    source.emit('tool_call', {
      type: 'tool_call',
      messageId: 9,
      name: 'generate_image',
      arguments: {},
      status: 'running',
    });
    source.emit('tool_result', {
      type: 'tool_result',
      messageId: 9,
      name: 'generate_image',
      ok: true,
      summary: '1 image',
    });

    await vi.waitFor(() => {
      expect(element('[data-message-id="9"] .tool-call').textContent).toContain('1 image');
    });
    expect(element('[data-message-id="9"] .tool-call').textContent).toContain('generate_image');
  });

  it('asset 事件渲染成图片', async () => {
    await boot({ conversationId: '5' });
    const source = openedStreams[0];

    source.emit('turn_started', { type: 'turn_started', messageId: 9, mode: 'image' });
    source.emit('asset', {
      type: 'asset',
      messageId: 9,
      id: 3,
      kind: 'image',
      url: '/api/v1/assets/3/download',
      mime: 'image/png',
      bytes: 1,
    });

    await vi.waitFor(() => {
      expect(element('[data-message-id="9"] img.asset-image').getAttribute('src')).toBe(
        '/api/v1/assets/3/download',
      );
    });
  });

  it('asset 事件按 kind 渲染成视频', async () => {
    await boot({ conversationId: '5' });
    const source = openedStreams[0];

    source.emit('turn_started', { type: 'turn_started', messageId: 9, mode: 'video' });
    source.emit('asset', {
      type: 'asset',
      messageId: 9,
      id: 4,
      kind: 'video',
      url: '/api/v1/assets/4/download',
      mime: 'video/mp4',
      bytes: 1,
    });

    await vi.waitFor(() => {
      expect(document.querySelector('[data-message-id="9"] video.asset-video')).not.toBeNull();
    });
  });

  it('turn_error 在那一轮上显示错误行并解锁输入框', async () => {
    await boot({ conversationId: '5' });
    const source = openedStreams[0];

    source.emit('turn_started', { type: 'turn_started', messageId: 9, mode: 'image' });
    source.emit('turn_error', { type: 'turn_error', messageId: 9, message: '出图失败' });

    await vi.waitFor(() => {
      expect(element('[data-message-id="9"] .turn-error').textContent).toBe('出图失败');
    });
    expect(element<HTMLButtonElement>('#send').disabled).toBe(false);
  });

  it('一轮在进行中锁住输入框，turn_done 到达才恢复', async () => {
    await boot();
    stubApi({ turns: { status: 202, body: { conversationId: 5, messageId: 9 } } });

    submit('画只隼');
    await vi.waitFor(() => {
      expect(element<HTMLButtonElement>('#send').disabled).toBe(true);
    });
    expect(element<HTMLTextAreaElement>('#prompt').disabled).toBe(true);

    openedStreams[0].emit('turn_done', { type: 'turn_done', messageId: 9 });
    await vi.waitFor(() => {
      expect(element<HTMLButtonElement>('#send').disabled).toBe(false);
    });
    expect(element<HTMLTextAreaElement>('#prompt').disabled).toBe(false);
  });

  it('别的页签起了轮次，这一页也锁：一轮一会话是服务端规则', async () => {
    await boot({ conversationId: '5' });
    const source = openedStreams[0];

    source.emit('turn_started', { type: 'turn_started', messageId: 42, mode: 'auto' });
    expect(element<HTMLButtonElement>('#send').disabled).toBe(true);

    source.emit('turn_done', { type: 'turn_done', messageId: 42 });
    await vi.waitFor(() => {
      expect(element<HTMLButtonElement>('#send').disabled).toBe(false);
    });
  });

  it('409 显示服务端的原因，而不是含糊的状态码', async () => {
    await boot();
    stubApi({ turns: { status: 409, body: { message: '该会话已有轮次在进行中' } } });

    submit('抢跑');
    await vi.waitFor(() => {
      expect(element('.turn-error').textContent).toBe('该会话已有轮次在进行中');
    });
  });

  it('非 2xx 且没有可读原因时退回 HTTP 状态码', async () => {
    await boot();
    stubApi({ turns: { status: 500, body: {} } });

    submit('画只隼');
    await vi.waitFor(() => {
      expect(element('.turn-error').textContent).toContain('500');
    });
  });

  it('全新页面：先建会话、以 from=0 开流，再提交轮次', async () => {
    await boot({ conversationId: '' });
    stubApi({
      create: { conversationId: 42 },
      turns: { status: 202, body: { conversationId: 42, messageId: 9 } },
    });

    submit('第一问');
    await vi.waitFor(() => {
      const urls = vi.mocked(fetch).mock.calls.map((call) => String(call[0]));
      expect(urls).toContain('/api/v1/conversations/42/turns');
    });

    const urls = vi.mocked(fetch).mock.calls.map((call) => String(call[0]));
    expect(urls[0]).toBe('/api/v1/conversations');
    // 流在提交轮次之前就开、且游标从 0 起：否则提交后立刻发布的事件会掉进
    // “还没连上”与“从最新开始”之间的缝里（POST 响应与 EventSource 连接赛跑）。
    expect(openedStreams.map((source) => source.url)).toEqual([
      '/api/v1/conversations/42/stream?from=0',
    ]);
    expect(document.body.dataset.conversationId).toBe('42');
    expect(window.location.search).toBe('?c=42');
  });

  it('会话已被删除（404）时重建并重发，恰发两次 turns', async () => {
    await boot({ conversationId: '5' });
    let turnsAttempts = 0;
    vi.stubGlobal(
      'fetch',
      vi.fn(async (url: string, init?: { method?: string }) => {
        const path = String(url);
        if (path === '/api/v1/conversations') {
          if ((init?.method ?? 'GET') === 'POST') {
            return { ok: true, status: 201, json: async () => ({ conversationId: 42 }) };
          }
          return { ok: true, status: 200, json: async () => [] };
        }
        if (/^\/api\/v1\/conversations\/\d+\/turns$/.test(path)) {
          turnsAttempts += 1;
          if (turnsAttempts === 1) {
            return { ok: false, status: 404, json: async () => ({ message: 'not found' }) };
          }
          return {
            ok: true,
            status: 202,
            json: async () => ({ conversationId: 42, messageId: 9 }),
          };
        }
        return { ok: true, status: 200, json: async () => [] };
      }),
    );

    submit('画只隼');
    await vi.waitFor(() => {
      expect(turnsAttempts).toBe(2);
    });

    expect(document.body.dataset.conversationId).toBe('42');
    // 旧会话的流被关掉，换成了新会话的、从 0 起的那条。
    const urls = openedStreams.map((source) => source.url);
    expect(urls).toEqual(['/api/v1/conversations/5/stream?from=55', '/api/v1/conversations/42/stream?from=0']);
    expect(openedStreams[0]?.closed).toBe(true);
    expect(openedStreams[1]?.closed).toBe(false);
  });

  it('resync（游标低于保留窗口）让页面重载，从库里的数据重建', async () => {
    await boot({ conversationId: '5' });
    const original = window.location;
    const reload = vi.fn();
    Object.defineProperty(window, 'location', {
      configurable: true,
      writable: true,
      value: { reload, pathname: '/', search: '' },
    });
    try {
      openedStreams[0].emit('resync', { type: 'resync', conversationId: 5, cursor: 0 });
      expect(reload).toHaveBeenCalledTimes(1);
    } finally {
      Object.defineProperty(window, 'location', { configurable: true, writable: true, value: original });
    }
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
    expect(deleteDialog().open).toBe(true);
    // 确认框里写着问的是哪一条，答案不会落错行
    expect(element('#conversation-delete-target').textContent).toContain('另一个会话');
  });

  it('点取消关掉确认框，也不发请求', async () => {
    await boot();
    stubDeleteFetch();

    deleteButton(6).click();
    deleteChoice('cancel').click();
    await Promise.resolve();

    expect(vi.mocked(fetch)).not.toHaveBeenCalled();
    expect(deleteDialog().open).toBe(false);
    expect(deleteButton(6)).not.toBeNull();
  });

  it('点确认才发请求，随后刷新列表', async () => {
    await boot();
    stubDeleteFetch();

    deleteButton(6).click();
    deleteChoice('confirm').click();
    await vi.waitFor(() => {
      expect(document.querySelector('button[data-delete-conversation="6"]')).toBeNull();
    });

    const requested = vi.mocked(fetch).mock.calls.map((call) => String(call[0]));
    expect(requested).toContain('/api/v1/conversations/6/delete');
    expect(requested).toContain('/api/v1/conversations');
    // 删的不是当前会话，页面不该被重置
    expect(document.body.dataset.conversationId).toBe('5');
  });

  it('删掉当前会话后回到新会话状态，并关掉事件流', async () => {
    await boot({ conversationId: '5' });
    stubDeleteFetch({ remaining: [{ id: 6, title: '另一个会话' }] });
    const stream = openedStreams[0];

    deleteButton(5).click();
    deleteChoice('confirm').click();
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
    deleteChoice('confirm').click();
    await vi.waitFor(() => {
      expect(document.querySelector('.conversation-error')).not.toBeNull();
    });

    expect(element('.conversation-error').textContent).toContain('500');
    expect(document.querySelector('button[data-delete-conversation="6"]')).not.toBeNull();
  });

  it('id 解析不出来的按钮不会打开确认框', async () => {
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
    await Promise.resolve();

    expect(deleteDialog().open).toBe(false);
    expect(vi.mocked(fetch)).not.toHaveBeenCalled();
  });

  it('不在列表项里的按钮什么都不做', async () => {
    // The row is where the question gets the title it names, so a button without
    // one cannot ask a sensible question — inert rather than half-working.
    await boot();
    stubDeleteFetch();
    const stray = document.createElement('button');
    stray.dataset.deleteConversation = '6';
    element('.conversation-list').append(stray);

    stray.click();
    await Promise.resolve();

    expect(deleteDialog().open).toBe(false);
    expect(vi.mocked(fetch)).not.toHaveBeenCalled();
  });

  it('一次确认只发一次请求', async () => {
    // The dialog is closed by its answer, so 删除 cannot be reached again by a
    // pointer. A test can still dispatch a click on the hidden button, which is
    // why the pending conversation is cleared on the click itself.
    await boot();
    stubDeleteFetch();

    deleteButton(6).click();
    deleteChoice('confirm').click();
    await vi.waitFor(() => {
      expect(deleteDialog().open).toBe(false);
    });

    deleteChoice('confirm').click();
    await Promise.resolve();

    const requested = vi.mocked(fetch).mock.calls.map((call) => String(call[0]));
    expect(requested.filter((url) => url === '/api/v1/conversations/6/delete')).toHaveLength(1);
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
 * server side instead — __tests__/server/view/view.controller.test.ts renders the real
 * template and asserts every id this file's fixture depends on is present.
 * Change a template id and that suite fails; this one keeps testing behaviour.
 */
