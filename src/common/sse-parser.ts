export interface SseFrame {
  event: string | null;
  data: string;
}

function parseFrame(frame: string): SseFrame | null {
  let event: string | null = null;
  const dataLines: string[] = [];

  for (const line of frame.split('\n')) {
    if (line.length === 0 || line.startsWith(':')) {
      // Comment / keep-alive line. The Bailian endpoint also emits
      // `:HTTP_STATUS/200` here, which is why this is skipped rather than parsed.
      continue;
    }
    if (line.startsWith('event:')) {
      event = line.slice('event:'.length).trim();
      continue;
    }
    if (line.startsWith('data:')) {
      dataLines.push(line.slice('data:'.length).replace(/^ /, ''));
    }
  }

  if (event === null && dataLines.length === 0) {
    return null;
  }
  return { event, data: dataLines.join('\n') };
}

/**
 * Splits a byte stream into SSE frames.
 *
 * Frames are separated by a blank line. The Bailian endpoint additionally emits
 * `id:N` lines and a non-standard `:HTTP_STATUS/200` comment line per frame,
 * both of which are tolerated here.
 */
export async function* parseSseStream(
  body: ReadableStream<Uint8Array>,
): AsyncGenerator<SseFrame> {
  const reader = body.getReader();
  const decoder = new TextDecoder();
  let buffer = '';

  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) {
        break;
      }
      buffer += decoder.decode(value, { stream: true });

      let boundary = buffer.indexOf('\n\n');
      while (boundary !== -1) {
        const raw = buffer.slice(0, boundary);
        buffer = buffer.slice(boundary + 2);
        const frame = parseFrame(raw);
        if (frame !== null) {
          yield frame;
        }
        boundary = buffer.indexOf('\n\n');
      }
    }

    const trailing = parseFrame(buffer);
    if (trailing !== null) {
      yield trailing;
    }
  } finally {
    reader.releaseLock();
  }
}
