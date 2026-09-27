import { beforeEach, describe, expect, it, vi } from 'vitest';

/**
 * Drives the voice page's client script through the DOM and the browser APIs it
 * uses, with a fake socket standing in for the network.
 *
 * This file exists because of a bug it would have caught: the client used to
 * open the socket and then send nothing at all, because the frame that starts a
 * call (`start`) was never written. The server opens no upstream session until
 * it arrives, so the page sat on "正在连接…" with a perfectly healthy socket and
 * a clean console — the failure was invisible from the outside. What is asserted
 * here is therefore the frame sequence, not just the absence of errors.
 */
const FIXTURE = `
  <main class="voice-stage">
    <p class="voice-status" id="voice-status">按住按钮开始说话</p>
    <ol class="voice-log" id="voice-log"></ol>
  </main>
  <div class="voice-controls">
    <button type="button" class="voice-button" id="voice-button">按住说话</button>
  </div>
`;

/** A parsed frame, narrowed without an assertion. */
function toRecord(value: unknown): Record<string, unknown> | null {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) {
    return null;
  }
  const record: Record<string, unknown> = {};
  for (const [key, item] of Object.entries(value)) {
    record[key] = item;
  }
  return record;
}

class FakeWebSocket {
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSING = 2;
  static readonly CLOSED = 3;
  static instances: FakeWebSocket[] = [];

  readonly url: string;
  readyState: number = FakeWebSocket.CONNECTING;
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: string }) => void) | null = null;
  onclose: (() => void) | null = null;
  closed = false;
  private readonly raw: string[] = [];

  constructor(url: string) {
    this.url = url;
    FakeWebSocket.instances.push(this);
  }

  send(payload: string): void {
    this.raw.push(payload);
  }

  close(): void {
    this.closed = true;
    this.readyState = FakeWebSocket.CLOSED;
  }

  /** What the client put on the wire, in order. */
  frames(): Record<string, unknown>[] {
    const parsed: Record<string, unknown>[] = [];
    for (const payload of this.raw) {
      const frame = toRecord(JSON.parse(payload));
      if (frame !== null) {
        parsed.push(frame);
      }
    }
    return parsed;
  }

  open(): void {
    this.readyState = FakeWebSocket.OPEN;
    this.onopen?.();
  }

  emit(payload: unknown): void {
    this.onmessage?.({ data: JSON.stringify(payload) });
  }
}

class FakeEventSource {
  static instances: FakeEventSource[] = [];
  readonly url: string;
  constructor(url: string) {
    this.url = url;
    FakeEventSource.instances.push(this);
  }
  addEventListener(): void {}
  close(): void {}
}

class FakeAudioWorkletNode {
  static instances: FakeAudioWorkletNode[] = [];
  port: { onmessage: ((event: { data: ArrayBuffer }) => void) | null } = { onmessage: null };
  constructor() {
    FakeAudioWorkletNode.instances.push(this);
  }
  connect(): void {}
  disconnect(): void {}
}

class FakeAudioContext {
  readonly sampleRate: number;
  readonly destination = {};
  readonly audioWorklet = { addModule: async (): Promise<void> => {} };
  constructor(options?: { sampleRate?: number }) {
    this.sampleRate = options?.sampleRate ?? 48000;
  }
  async resume(): Promise<void> {}
  createMediaStreamSource(): { connect: () => void } {
    return { connect: (): void => {} };
  }
}

function stubTicket(options: { ok?: boolean; ticket?: string } = {}): void {
  const ok = options.ok ?? true;
  const ticket = options.ticket ?? 'ticket-1';
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => ({
      ok,
      status: ok ? 201 : 401,
      json: async () => ({ ticket, expiresAt: Date.now() + 60_000 }),
    })),
  );
}

async function boot(): Promise<void> {
  vi.resetModules();
  document.body.innerHTML = FIXTURE;
  FakeWebSocket.instances = [];
  FakeAudioWorkletNode.instances = [];
  FakeEventSource.instances = [];
  vi.stubGlobal('WebSocket', FakeWebSocket);
  vi.stubGlobal('EventSource', FakeEventSource);
  vi.stubGlobal('AudioContext', FakeAudioContext);
  vi.stubGlobal('AudioWorkletNode', FakeAudioWorkletNode);
  Object.defineProperty(navigator, 'mediaDevices', {
    configurable: true,
    value: { getUserMedia: async () => ({ getTracks: () => [{ stop: (): void => {} }] }) },
  });
  URL.createObjectURL = (): string => 'blob:test';
  URL.revokeObjectURL = (): void => {};
  await import('./voice');
}

function element<T extends Element>(selector: string): T {
  const found = document.querySelector<T>(selector);
  if (found === null) {
    throw new Error(`fixture is missing ${selector}`);
  }
  return found;
}

function button(): HTMLButtonElement {
  return element<HTMLButtonElement>('#voice-button');
}

function statusText(): string {
  return document.querySelector('#voice-status')?.textContent ?? '';
}

/** pointerdown as a plain Event: the handler only reads the type. */
function press(): void {
  button().dispatchEvent(new Event('pointerdown', { bubbles: true, cancelable: true }));
}

function release(): void {
  button().dispatchEvent(new Event('pointerup', { bubbles: true, cancelable: true }));
}

async function socketAfterPress(): Promise<FakeWebSocket> {
  press();
  await vi.waitFor(() => {
    expect(FakeWebSocket.instances).toHaveLength(1);
  });
  const socket = FakeWebSocket.instances[0];
  if (socket === undefined) {
    throw new Error('press did not open a socket');
  }
  return socket;
}

/** A chunk of microphone audio, as the worklet would deliver it. */
function emitMicChunk(): void {
  const node = FakeAudioWorkletNode.instances[0];
  node?.port.onmessage?.({ data: new ArrayBuffer(4) });
}

beforeEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe('开一次通话', () => {
  it('按下后先取票，再带着票据连 /api/voice', async () => {
    stubTicket({ ticket: 'abc123' });
    await boot();

    const socket = await socketAfterPress();

    expect(vi.mocked(fetch).mock.calls[0]?.[0]).toBe('/api/voice/ticket');
    expect(socket.url).toContain('/api/voice?ticket=abc123');
  });

  it('socket 一打开就发 start —— 服务端只认这一帧才会开会话', async () => {
    // The regression: without it the server never opens the upstream session,
    // never answers `ready`, and the page waits forever on "正在连接…".
    stubTicket();
    await boot();
    const socket = await socketAfterPress();

    expect(socket.frames()).toEqual([]);
    socket.open();

    expect(socket.frames()).toEqual([{ type: 'start' }]);
  });

  it('拿不到票据时不连 socket，并说明原因', async () => {
    stubTicket({ ok: false });
    await boot();

    press();
    await vi.waitFor(() => {
      expect(document.querySelector('.voice-entry-error')).not.toBeNull();
    });

    expect(FakeWebSocket.instances).toEqual([]);
    expect(document.querySelector('.voice-entry-error')?.textContent).toContain('401');
  });
});

describe('会话就绪前后', () => {
  it('ready 之前是「正在连接…」，ready 到达且仍按住时变成「正在听…」', async () => {
    stubTicket();
    await boot();
    const socket = await socketAfterPress();

    expect(statusText()).toBe('正在连接…');

    socket.emit({ type: 'ready', conversationId: 7 });

    expect(statusText()).toBe('正在听…');
  });

  it('ready 之前录到的音频先排队，ready 后补发', async () => {
    stubTicket();
    await boot();
    const socket = await socketAfterPress();
    socket.open();

    emitMicChunk();
    expect(socket.frames().map((frame) => frame.type)).toEqual(['start']);

    socket.emit({ type: 'ready', conversationId: 7 });

    expect(socket.frames().map((frame) => frame.type)).toEqual(['start', 'audio']);
  });

  it('ready 到达后打开该会话的事件流', async () => {
    // Video results arrive on the conversation stream, not on the voice socket.
    stubTicket();
    await boot();
    const socket = await socketAfterPress();
    socket.open();

    socket.emit({ type: 'ready', conversationId: 7 });

    expect(FakeEventSource.instances.map((source) => source.url)).toEqual([
      '/api/conversations/7/events',
    ]);
  });

  it('ready 之前松开按钮时，把 commit 记下来等就绪后补发', async () => {
    stubTicket();
    await boot();
    const socket = await socketAfterPress();
    socket.open();

    release();
    // Handshake done, session not up: only `start` has gone out.
    expect(socket.frames().map((frame) => frame.type)).toEqual(['start']);

    socket.emit({ type: 'ready', conversationId: 7 });

    expect(socket.frames().map((frame) => frame.type)).toEqual(['start', 'commit']);
  });

  it('ready 之后松开按钮立刻发 commit', async () => {
    stubTicket();
    await boot();
    const socket = await socketAfterPress();
    socket.open();
    socket.emit({ type: 'ready', conversationId: 7 });

    release();

    expect(socket.frames().map((frame) => frame.type)).toEqual(['start', 'commit']);
    expect(statusText()).toBe('正在思考…');
  });
});

/**
 * The endpoint refuses audio past 30s of uncommitted buffer, and then refuses
 * every further chunk — which reaches the page as a screenful of errors. The
 * hold is therefore cut short of the limit.
 */
describe('单段时长上限', () => {
  /**
   * A press whose session is up, with the clock in hand: the limit timer is
   * armed by the `ready` frame, so the fake clock has to be installed before it
   * arrives or the timer is a real one and advancing does nothing.
   */
  async function readyPress(): Promise<FakeWebSocket> {
    const socket = await socketAfterPress();
    socket.open();
    vi.useFakeTimers();
    socket.emit({ type: 'ready', conversationId: 7 });
    return socket;
  }

  function framesOf(socket: FakeWebSocket): string[] {
    return socket.frames().map((frame) => String(frame.type));
  }

  it('到上限自动提交一次，之后不再送音频，并给用户一句说明', async () => {
    stubTicket();
    await boot();
    const socket = await readyPress();
    emitMicChunk();
    expect(framesOf(socket)).toEqual(['start', 'audio']);

    vi.advanceTimersByTime(25_000);
    vi.useRealTimers();

    expect(framesOf(socket)).toEqual(['start', 'audio', 'commit']);
    expect(element('.voice-entry-note').textContent).toContain('上限');

    // 之后这段按住里采到的音频不再送，松开也不重复提交
    emitMicChunk();
    release();
    expect(framesOf(socket)).toEqual(['start', 'audio', 'commit']);
  });

  it('没到上限就松开时不受影响', async () => {
    stubTicket();
    await boot();
    const socket = await readyPress();
    emitMicChunk();

    vi.advanceTimersByTime(10_000);
    vi.useRealTimers();
    release();

    expect(framesOf(socket)).toEqual(['start', 'audio', 'commit']);
  });

  it('收到 error 后本次按住不再送音频，松开也不提交空缓冲', async () => {
    stubTicket();
    await boot();
    const socket = await socketAfterPress();
    socket.open();
    socket.emit({ type: 'ready', conversationId: 7 });
    emitMicChunk();

    socket.emit({ type: 'error', message: 'Input audio buffer exceeded maximum duration (30s).' });
    expect(element('.voice-entry-error').textContent).toContain('30s');

    emitMicChunk();
    release();
    expect(framesOf(socket)).toEqual(['start', 'audio']);
  });

  it('本轮说完之后，下一次按住重新开始计时并恢复送音频', async () => {
    stubTicket();
    await boot();
    const socket = await readyPress();

    vi.advanceTimersByTime(25_000);
    vi.useRealTimers();
    emitMicChunk();
    release();
    // 上限之后的按住里不能再按，直到这一轮真正结束
    expect(statusText()).toBe('正在思考…');
    socket.emit({ type: 'done' });

    press();
    await vi.waitFor(() => {
      expect(statusText()).toBe('正在听…');
    });
    emitMicChunk();

    expect(framesOf(socket)).toEqual(['start', 'commit', 'audio']);
  });
});
