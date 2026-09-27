/**
 * The slice of an HTTP response this writer actually touches.
 *
 * Declared structurally rather than as `express.Response` so the writer stays
 * free of the framework and can be driven by a plain object in tests. An
 * express Response satisfies it as-is.
 */
export interface SseResponse {
  writeHead(statusCode: number, headers: Record<string, string>): void;
  flushHeaders(): void;
  write(chunk: string): void;
  end(): void;
}

/** Event names used by the agent stream. Mirrors docs/architecture.md. */
export type SseEventName =
  | 'connected'
  | 'reasoning'
  | 'text'
  | 'tool_call'
  | 'tool_result'
  | 'asset'
  | 'done'
  | 'error'
  // Conversation-level stream (background jobs, not tied to a turn).
  | 'task_updated'
  | 'message_added';

const HEARTBEAT_MS = 15_000;

/**
 * Writes an SSE stream onto a raw express response.
 *
 * Image generation leaves the connection silent for tens of seconds, so a
 * comment frame is sent on an interval to keep proxies and browsers from
 * treating the stream as idle and closing it.
 */
export class SseWriter {
  private heartbeat: NodeJS.Timeout | null = null;
  private closed = false;

  constructor(private readonly res: SseResponse) {}

  open(): void {
    this.res.writeHead(200, {
      'Content-Type': 'text/event-stream; charset=utf-8',
      'Cache-Control': 'no-cache, no-transform',
      Connection: 'keep-alive',
      'X-Accel-Buffering': 'no',
    });
    this.res.flushHeaders();
    this.heartbeat = setInterval(() => {
      if (!this.closed) {
        this.res.write(': ping\n\n');
      }
    }, HEARTBEAT_MS);
  }

  send(event: SseEventName, data: unknown): void {
    if (this.closed) {
      return;
    }
    this.res.write(`event: ${event}\ndata: ${JSON.stringify(data)}\n\n`);
  }

  close(): void {
    if (this.closed) {
      return;
    }
    this.closed = true;
    if (this.heartbeat !== null) {
      clearInterval(this.heartbeat);
      this.heartbeat = null;
    }
    this.res.end();
  }
}
