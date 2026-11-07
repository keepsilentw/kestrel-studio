import { EventEmitter } from 'node:events';
import { describe, expect, it } from 'vitest';
import {
  createAccessLogMiddleware,
  type AccessLogRequest,
  type AccessLogResponse,
} from '@/common/access-log';

/**
 * Pins the forensic shape that makes a read-only pass visible on the server at all
 * (docs/verification.md §13.6, and the Rust counterpart in
 * `rust/server/src/http/router.rs`): one line per request, query string kept, and
 * exactly one line even when the response ends by the client going away.
 */

class FakeResponse extends EventEmitter implements AccessLogResponse {
  statusCode = 200;
}

function fakeRequest(method: string, originalUrl: string): AccessLogRequest {
  return { method, originalUrl };
}

/** A clock that advances by the given steps on each read. */
function steppedClock(start: number, ...steps: number[]): () => number {
  let current = start;
  let index = 0;
  return () => {
    const value = current;
    current += steps[Math.min(index, steps.length - 1)] ?? 0;
    index += 1;
    return value;
  };
}

describe('createAccessLogMiddleware', () => {
  it('writes one line with method, uri, status and latency', () => {
    const lines: string[] = [];
    const middleware = createAccessLogMiddleware({
      enabled: true,
      write: (line) => lines.push(line),
      now: steppedClock(1_000, 3),
    });
    const res = new FakeResponse();
    res.statusCode = 200;

    middleware(fakeRequest('GET', '/login?probe=1'), res, () => undefined);
    res.emit('finish');

    expect(lines).toEqual(['method=GET uri=/login?probe=1 status=200 latency=3ms']);
  });

  it('keeps the query string, which is the whole point for a resumed stream', () => {
    const lines: string[] = [];
    const middleware = createAccessLogMiddleware({
      enabled: true,
      write: (line) => lines.push(line),
      now: () => 0,
    });

    // The line appears when the response ends, not when the request starts.
    const res = new FakeResponse();
    res.statusCode = 401;
    middleware(fakeRequest('GET', '/api/v1/conversations/7/stream?from=9'), res, () => undefined);
    expect(lines).toEqual([]);
    res.emit('finish');
    expect(lines).toEqual([
      'method=GET uri=/api/v1/conversations/7/stream?from=9 status=401 latency=0ms',
    ]);
  });

  it('writes nothing and still calls next while the gate is off', () => {
    const lines: string[] = [];
    let nextCalls = 0;
    const middleware = createAccessLogMiddleware({
      enabled: false,
      write: (line) => lines.push(line),
      now: () => 0,
    });
    const res = new FakeResponse();

    middleware(fakeRequest('GET', '/login'), res, () => {
      nextCalls += 1;
    });
    res.emit('finish');

    expect(nextCalls).toBe(1);
    expect(lines).toEqual([]);
  });

  it('emits a single line when finish and close both fire (aborted SSE)', () => {
    const lines: string[] = [];
    const middleware = createAccessLogMiddleware({
      enabled: true,
      write: (line) => lines.push(line),
      now: () => 0,
    });
    const res = new FakeResponse();
    res.statusCode = 200;

    middleware(fakeRequest('GET', '/api/v1/conversations/7/stream?from=0'), res, () => undefined);
    res.emit('close');
    res.emit('finish');

    expect(lines).toHaveLength(1);
    expect(lines[0]).toContain('uri=/api/v1/conversations/7/stream?from=0');
  });

  it('clamps a clock that stepped backwards to zero latency', () => {
    const lines: string[] = [];
    const middleware = createAccessLogMiddleware({
      enabled: true,
      write: (line) => lines.push(line),
      now: steppedClock(1_000, -50),
    });
    const res = new FakeResponse();
    res.statusCode = 302;

    middleware(fakeRequest('POST', '/logout'), res, () => undefined);
    res.emit('finish');

    expect(lines).toEqual(['method=POST uri=/logout status=302 latency=0ms']);
  });

  it('works on the default sink and clock without throwing', () => {
    const middleware = createAccessLogMiddleware({ enabled: true });
    const res = new FakeResponse();
    let nextCalls = 0;

    middleware(fakeRequest('GET', '/'), res, () => {
      nextCalls += 1;
    });
    res.emit('finish');

    expect(nextCalls).toBe(1);
  });
});
