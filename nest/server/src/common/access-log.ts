import { performance } from 'node:perf_hooks';
import { Logger } from '@nestjs/common';
import type { EventEmitter } from 'node:events';

/**
 * One access line per request: method, uri (query string kept), status, latency.
 *
 * This is the Nest counterpart of the Rust server's `tower_http::trace::TraceLayer`
 * (`rust/server/src/http/router.rs`), added so both online implementations leave the
 * same forensic trace: a read-only pass — page turns, `/admin` replays, an SSE resume
 * at `?from=N` — is otherwise invisible server side (docs/verification.md §13.6).
 *
 * Two deliberate differences from the Rust side, both recorded in
 * docs/architecture.md §13.3:
 *
 * 1. `/api/voice` needs no exclusion here. In Rust the WebSocket upgrade is an axum
 *    route, so it had to be registered *after* `.layer()` to keep the tracer off the
 *    hijacked connection. In Nest the upgrade never reaches Express at all —
 *    `voice/voice.gateway.ts` listens on the HTTP server's `upgrade` event — so this
 *    middleware structurally cannot see it. `/api/voice/ticket` is a normal POST and
 *    is logged, as in Rust.
 * 2. `latency` is measured to `finish`, i.e. the whole response, whereas tower's
 *    `on_response` fires when the head goes out (time-to-first-byte on the two
 *    streaming routes). So for `/api/v1/conversations/{id}/stream` the Nest number is
 *    the length of the stream and the Rust number is not.
 */

/** The request fields this middleware reads. Express's `Request` satisfies it. */
export interface AccessLogRequest {
  readonly method: string;
  readonly originalUrl: string;
}

/** The response fields this middleware reads. Express's `Response` satisfies it. */
export interface AccessLogResponse extends EventEmitter {
  readonly statusCode: number;
}

export interface AccessLogOptions {
  /**
   * False keeps the middleware a pass-through that writes nothing. This is the gate —
   * not a logger level — so a deployment cannot turn the lines on by accident. The
   * Rust counterpart is `RUST_LOG=debug`; here it is `ACCESS_LOG=1`, read by
   * `loadConfig()` (docs/deployment.md §2.1).
   */
  readonly enabled: boolean;
  /** Injectable sink; defaults to the Nest `verbose` channel. */
  readonly write?: (line: string) => void;
  /** Injectable clock so latency is assertable without waiting. */
  readonly now?: () => number;
}

const logger = new Logger('access');

/** Builds the Express middleware. Kept a factory so it is testable without a server. */
export function createAccessLogMiddleware(
  options: AccessLogOptions,
): (req: AccessLogRequest, res: AccessLogResponse, next: (...args: unknown[]) => void) => void {
  const write = options.write ?? ((line: string): void => logger.verbose(line));
  const now = options.now ?? ((): number => performance.now());

  return (req, res, next): void => {
    if (!options.enabled) {
      next();
      return;
    }

    const startedAt = now();
    let emitted = false;
    const emit = (): void => {
      // `finish` fires on a normal response; `close` also fires when the client goes
      // away mid-stream. An aborted SSE is exactly the event a resume investigation
      // looks for, so it has to leave a line — but only one line.
      if (emitted) {
        return;
      }
      emitted = true;
      const latency = Math.max(0, Math.round(now() - startedAt));
      write(`method=${req.method} uri=${req.originalUrl} status=${res.statusCode} latency=${latency}ms`);
    };

    res.once('finish', emit);
    res.once('close', emit);
    next();
  };
}
