import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { parseSseStream, type SseFrame } from '@/common/sse-parser';
import { SseWriter, type SseResponse } from '@/common/sse';

const HEARTBEAT_MS = 15_000;

interface Recorded {
  statusCode: number | null;
  headers: Record<string, string> | null;
  flushes: number;
  written: string[];
  ends: number;
}

/**
 * A plain object rather than a mocked express Response — the writer only needs
 * four methods, and SseResponse declares exactly those.
 */
function recordingResponse(): { res: SseResponse; rec: Recorded } {
  const rec: Recorded = {
    statusCode: null,
    headers: null,
    flushes: 0,
    written: [],
    ends: 0,
  };
  const res: SseResponse = {
    writeHead(statusCode, headers): void {
      rec.statusCode = statusCode;
      rec.headers = headers;
    },
    flushHeaders(): void {
      rec.flushes += 1;
    },
    write(chunk): void {
      rec.written.push(chunk);
    },
    end(): void {
      rec.ends += 1;
    },
  };
  return { res, rec };
}

/** Feeds the writer's output back through the reader to close the loop. */
async function framesFrom(text: string): Promise<SseFrame[]> {
  const bytes = new TextEncoder().encode(text);
  const body = new ReadableStream<Uint8Array>({
    start(controller): void {
      controller.enqueue(bytes);
      controller.close();
    },
  });
  const frames: SseFrame[] = [];
  for await (const frame of parseSseStream(body)) {
    frames.push(frame);
  }
  return frames;
}

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.clearAllTimers();
  vi.useRealTimers();
});

describe('SseWriter.open', () => {
  it('写 200 与 SSE 必需的响应头，然后 flush', () => {
    const { res, rec } = recordingResponse();
    new SseWriter(res).open();

    expect(rec.statusCode).toBe(200);
    expect(rec.headers?.['Content-Type']).toBe('text/event-stream; charset=utf-8');
    expect(rec.headers?.['Cache-Control']).toBe('no-cache, no-transform');
    expect(rec.headers?.Connection).toBe('keep-alive');
    // Without this an nginx-style proxy buffers the whole response and the
    // streaming degenerates into one dump at the end. See docs/deployment.md §3.
    expect(rec.headers?.['X-Accel-Buffering']).toBe('no');
    expect(rec.flushes).toBe(1);
  });

  it('open 本身不写任何帧', () => {
    const { res, rec } = recordingResponse();
    new SseWriter(res).open();
    expect(rec.written).toEqual([]);
  });
});

describe('SseWriter.send', () => {
  it('产出的帧就是解析器读得回的帧', async () => {
    const { res, rec } = recordingResponse();
    const writer = new SseWriter(res);
    writer.open();
    writer.send('asset', { id: 7, kind: 'image' });

    const frames = await framesFrom(rec.written[0]);
    expect(frames).toHaveLength(1);
    expect(frames[0].event).toBe('asset');
    expect(JSON.parse(frames[0].data)).toEqual({ id: 7, kind: 'image' });
  });

  it('data 里的换行不会把一帧拆成两帧', async () => {
    // Model output routinely contains newlines. JSON.stringify escapes them,
    // which is what keeps a text delta from tearing the frame apart.
    const { res, rec } = recordingResponse();
    const writer = new SseWriter(res);
    writer.open();
    writer.send('text', { delta: '第一行\n第二行' });

    const frames = await framesFrom(rec.written[0]);
    expect(frames).toHaveLength(1);
    expect(JSON.parse(frames[0].data)).toEqual({ delta: '第一行\n第二行' });
  });

  it('连续多次 send 各自成帧，顺序保持', async () => {
    const { res, rec } = recordingResponse();
    const writer = new SseWriter(res);
    writer.open();
    writer.send('reasoning', { delta: 'a' });
    writer.send('text', { delta: 'b' });
    writer.send('done', { messageId: 1 });

    const frames = await framesFrom(rec.written.join(''));
    expect(frames.map((frame) => frame.event)).toEqual(['reasoning', 'text', 'done']);
  });
});

describe('SseWriter 心跳', () => {
  it('每 15s 发一次注释帧', () => {
    const { res, rec } = recordingResponse();
    new SseWriter(res).open();

    vi.advanceTimersByTime(HEARTBEAT_MS - 1);
    expect(rec.written).toEqual([]);

    vi.advanceTimersByTime(1);
    expect(rec.written).toEqual([': ping\n\n']);

    vi.advanceTimersByTime(HEARTBEAT_MS * 2);
    expect(rec.written).toHaveLength(3);
  });

  it('心跳帧是注释行，不会污染事件流', async () => {
    const { res, rec } = recordingResponse();
    const writer = new SseWriter(res);
    writer.open();
    writer.send('text', { delta: 'hi' });

    vi.advanceTimersByTime(HEARTBEAT_MS * 3);
    const frames = await framesFrom(rec.written.join(''));
    expect(frames).toEqual([{ event: 'text', data: '{"delta":"hi"}' }]);
  });
});

describe('SseWriter.close', () => {
  it('停掉心跳并结束响应', () => {
    const { res, rec } = recordingResponse();
    const writer = new SseWriter(res);
    writer.open();
    writer.close();

    expect(rec.ends).toBe(1);

    const afterClose = rec.written.length;
    vi.advanceTimersByTime(HEARTBEAT_MS * 4);
    expect(rec.written).toHaveLength(afterClose);
  });

  it('close 之后 send 变成空操作，且不抛', () => {
    const { res, rec } = recordingResponse();
    const writer = new SseWriter(res);
    writer.open();
    writer.close();

    expect(() => writer.send('text', { delta: 'late' })).not.toThrow();
    expect(rec.written).toEqual([]);
  });

  it('重复 close 只结束一次', () => {
    const { res, rec } = recordingResponse();
    const writer = new SseWriter(res);
    writer.open();
    writer.close();
    writer.close();

    expect(rec.ends).toBe(1);
  });

  it('没 open 就 close 也不抛', () => {
    const { res, rec } = recordingResponse();
    expect(() => new SseWriter(res).close()).not.toThrow();
    expect(rec.ends).toBe(1);
  });
});
