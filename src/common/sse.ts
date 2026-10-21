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

/**
 * Event names carried by the conversation stream. Mirrors docs/architecture.md
 * §6. Everything except `connected` and `resync` is journaled in
 * `conversation_events` and therefore replayable on reconnect; the two
 * connection-scoped names never carry an id.
 */
export type SseEventName =
  // Connection-scoped: describe this stream, not the conversation's history.
  | 'connected'
  | 'resync'
  // Turn events: journaled as the agent loop emits them.
  | 'user_message'
  | 'turn_started'
  | 'reasoning'
  | 'text'
  | 'tool_call'
  | 'tool_result'
  | 'asset'
  | 'turn_done'
  | 'turn_error'
  // Background jobs and their appended results.
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

  /**
   * `seq` is the journal id of a journaled event. It becomes the SSE `id:`
   * field, so a browser reconnecting an EventSource sends it back as
   * Last-Event-ID and the stream replays from there. Connection-scoped frames
   * (`connected`, `resync`) omit it — they must not move the client's cursor.
   */
  send(event: SseEventName, data: unknown, seq?: number): void {
    if (this.closed) {
      return;
    }
    const idLine = seq === undefined ? '' : `id: ${seq}\n`;
    this.res.write(`${idLine}event: ${event}\ndata: ${JSON.stringify(data)}\n\n`);
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
