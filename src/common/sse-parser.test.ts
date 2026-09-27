import { describe, expect, it } from 'vitest';
import { parseSseStream, type SseFrame } from '@/common/sse-parser';

const encoder = new TextEncoder();

/**
 * One enqueued chunk per read(), so a frame can be split across chunks
 * deterministically instead of depending on how the stream happens to buffer.
 */
function streamFrom(parts: readonly Uint8Array[]): ReadableStream<Uint8Array> {
  return new ReadableStream<Uint8Array>({
    start(controller): void {
      for (const part of parts) {
        controller.enqueue(part);
      }
      controller.close();
    },
  });
}

async function collect(parts: readonly Uint8Array[]): Promise<SseFrame[]> {
  const frames: SseFrame[] = [];
  for await (const frame of parseSseStream(streamFrom(parts))) {
    frames.push(frame);
  }
  return frames;
}

function byString(chunks: readonly string[]): Promise<SseFrame[]> {
  return collect(chunks.map((chunk) => encoder.encode(chunk)));
}

/** Encodes once, then slices the bytes — the only way to cut mid-character. */
function byBytes(payload: string, cuts: readonly number[]): Promise<SseFrame[]> {
  const bytes = encoder.encode(payload);
  const parts: Uint8Array[] = [];
  let start = 0;
  for (const cut of cuts) {
    parts.push(bytes.slice(start, cut));
    start = cut;
  }
  parts.push(bytes.slice(start));
  return collect(parts);
}

describe('parseSseStream', () => {
  it('解析一个完整帧', async () => {
    await expect(byString(['event: text\ndata: {"delta":"hi"}\n\n'])).resolves.toEqual([
      { event: 'text', data: '{"delta":"hi"}' },
    ]);
  });

  it('同一次 read 里的多个帧全部吐出', async () => {
    const frames = await byString(['event: a\ndata: 1\n\nevent: b\ndata: 2\n\n']);
    expect(frames).toEqual([
      { event: 'a', data: '1' },
      { event: 'b', data: '2' },
    ]);
  });

  it('帧被切成两个 chunk 仍能拼回', async () => {
    const frames = await byString(['event: text\ndata: {"del', 'ta":"hi"}\n\n']);
    expect(frames).toEqual([{ event: 'text', data: '{"delta":"hi"}' }]);
  });

  it('分隔符 \\n\\n 本身被切开也不会漏帧或多帧', async () => {
    const payload = 'event: text\ndata: 1\n\n';
    const cut = encoder.encode('event: text\ndata: 1\n').length;
    const frames = await byBytes(payload, [cut]);
    expect(frames).toEqual([{ event: 'text', data: '1' }]);
  });

  it('跨 chunk 切断多字节字符不乱码', async () => {
    const payload = 'event: text\ndata: {"delta":"红隼"}\n\n';
    // 红 is three bytes in UTF-8; land the cut one byte into it.
    const cut = encoder.encode('event: text\ndata: {"delta":"').length + 1;
    const frames = await byBytes(payload, [cut]);
    expect(frames).toEqual([{ event: 'text', data: '{"delta":"红隼"}' }]);
  });

  it('容忍百炼端点每帧附带的 :HTTP_STATUS/200 与 id:N 行', async () => {
    const frames = await byString([
      ':HTTP_STATUS/200\nid:1\nevent: text\ndata: {"delta":"hi"}\n\n',
    ]);
    expect(frames).toEqual([{ event: 'text', data: '{"delta":"hi"}' }]);
  });

  it('只有注释行（心跳）不产生帧', async () => {
    await expect(byString([': ping\n\n'])).resolves.toEqual([]);
  });

  it('多行 data 用换行连接', async () => {
    const frames = await byString(['event: text\ndata: line1\ndata: line2\n\n']);
    expect(frames).toEqual([{ event: 'text', data: 'line1\nline2' }]);
  });

  it('data 的冒号后只吃掉一个空格，其余原样保留', async () => {
    const frames = await byString(['data:  two\n\n']);
    expect(frames).toEqual([{ event: null, data: ' two' }]);
  });

  it('没有 event 字段时 event 为 null', async () => {
    const frames = await byString(['data: only\n\n']);
    expect(frames).toEqual([{ event: null, data: 'only' }]);
  });

  it('结尾帧没有空行收尾也要吐出', async () => {
    // The provider closes the stream right after the final frame, so the
    // trailing payload never gets its blank-line separator.
    await expect(byString(['event: done\ndata: {}'])).resolves.toEqual([
      { event: 'done', data: '{}' },
    ]);
  });

  it('连续空行不产生空帧', async () => {
    const frames = await byString(['event: a\ndata: 1\n\n\n\nevent: b\ndata: 2\n\n']);
    expect(frames).toEqual([
      { event: 'a', data: '1' },
      { event: 'b', data: '2' },
    ]);
  });

  it('空流不产出任何帧', async () => {
    await expect(byString([])).resolves.toEqual([]);
    await expect(byString([''])).resolves.toEqual([]);
  });
});
