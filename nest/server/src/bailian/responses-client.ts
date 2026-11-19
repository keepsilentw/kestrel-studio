import { Injectable, Logger } from '@nestjs/common';
import { parseSseStream, type SseFrame } from '@/common/sse-parser';
import { loadConfig } from '@/config/configuration';
import { resolveApiKey } from './token';

export interface ResponsesFunctionTool {
  type: 'function';
  name: string;
  description: string;
  parameters: Record<string, unknown>;
}

/**
 * Responses API input items. The function_call / function_call_output pair is
 * how a tool round is replayed: the call must be echoed back before its result,
 * since this loop is stateless (no previous_response_id).
 */
export type ResponsesInputItem =
  | { role: 'user' | 'assistant' | 'system'; content: string }
  | { type: 'function_call'; call_id: string; name: string; arguments: string }
  | { type: 'function_call_output'; call_id: string; output: string };

export type StreamEvent =
  | { kind: 'reasoning'; delta: string }
  | { kind: 'text'; delta: string }
  | { kind: 'function_call'; callId: string; name: string; arguments: string }
  | { kind: 'failed'; message: string };

export interface StreamOptions {
  input: ResponsesInputItem[];
  tools?: ResponsesFunctionTool[];
  instructions?: string;
  signal?: AbortSignal;
}

interface PendingCall {
  callId: string;
  name: string;
  args: string;
}

/** The provider answers headers quickly; a connection that never does would hold a turn slot forever. */
const CONNECT_TIMEOUT_MS = 60_000;

/**
 * Image rounds can be silent for tens of seconds. Three minutes with no bytes at
 * all is a half-open stream, not a slow render; end the turn with `turn_error`
 * instead of leaving the page locked on "正在生成…".
 */
const STREAM_IDLE_TIMEOUT_MS = 180_000;

@Injectable()
export class BailianResponsesClient {
  private readonly logger = new Logger(BailianResponsesClient.name);

  async *stream(options: StreamOptions): AsyncGenerator<StreamEvent> {
    const { bailian } = loadConfig();
    const controller = new AbortController();
    const forwardAbort = (): void => {
      controller.abort();
    };
    if (options.signal !== undefined) {
      if (options.signal.aborted) {
        controller.abort();
      } else {
        options.signal.addEventListener('abort', forwardAbort, { once: true });
      }
    }

    let timedOut = false;
    let idleTimer: ReturnType<typeof setTimeout> | undefined;
    const clearIdle = (): void => {
      if (idleTimer !== undefined) {
        clearTimeout(idleTimer);
        idleTimer = undefined;
      }
    };
    const armIdle = (): void => {
      clearIdle();
      idleTimer = setTimeout(() => {
        timedOut = true;
        controller.abort();
      }, STREAM_IDLE_TIMEOUT_MS);
    };

    try {
      const connectTimer = setTimeout(() => {
        timedOut = true;
        controller.abort();
      }, CONNECT_TIMEOUT_MS);
      let response: Response;
      try {
        response = await fetch(`${bailian.baseUrl}/compatible-mode/v1/responses`, {
          method: 'POST',
          headers: {
            Authorization: `Bearer ${resolveApiKey()}`,
            'Content-Type': 'application/json',
            Accept: 'text/event-stream',
          },
          body: JSON.stringify({
            model: bailian.chatModel,
            input: options.input,
            stream: true,
            reasoning: { effort: bailian.reasoningEffort },
            ...(options.tools !== undefined && options.tools.length > 0
              ? { tools: options.tools, tool_choice: 'auto' }
              : {}),
            ...(options.instructions !== undefined
              ? { instructions: options.instructions }
              : {}),
          }),
          signal: controller.signal,
        });
      } catch (error) {
        if (timedOut && options.signal?.aborted !== true) {
          throw new Error(
            `模型响应在 ${CONNECT_TIMEOUT_MS / 1000} 秒内没有开始`,
          );
        }
        throw error;
      } finally {
        clearTimeout(connectTimer);
      }

      if (!response.ok) {
        const raw = await response.text();
        throw new Error(
          `Bailian request failed (HTTP ${response.status}): ${raw.slice(0, 300) || 'no body'}`,
        );
      }
      if (response.body === null) {
        throw new Error('Bailian returned an empty stream.');
      }

      let pending: PendingCall | null = null;
      const iterator = parseSseStream(response.body)[Symbol.asyncIterator]();

      while (true) {
        armIdle();
        let next: IteratorResult<SseFrame>;
        try {
          next = await iterator.next();
        } catch (error) {
          if (timedOut && options.signal?.aborted !== true) {
            throw new Error(
              `模型响应超过 ${STREAM_IDLE_TIMEOUT_MS / 1000} 秒没有新数据`,
            );
          }
          throw error;
        } finally {
          clearIdle();
        }
        if (next.done === true) {
          break;
        }
        const frame = next.value;
        if (frame.event === null) {
          continue;
        }

        let payload: Record<string, unknown>;
        try {
          payload = JSON.parse(frame.data) as Record<string, unknown>;
        } catch {
          this.logger.warn(`Skipping unparsable SSE frame for event ${frame.event}`);
          continue;
        }

        switch (frame.event) {
          case 'response.reasoning_text.delta': {
            const delta = payload.delta;
            if (typeof delta === 'string' && delta.length > 0) {
              yield { kind: 'reasoning', delta };
            }
            break;
          }

          case 'response.output_text.delta': {
            const delta = payload.delta;
            if (typeof delta === 'string' && delta.length > 0) {
              yield { kind: 'text', delta };
            }
            break;
          }

          case 'response.output_item.added': {
            const item = payload.item as Record<string, unknown> | undefined;
            if (item?.type === 'function_call') {
              pending = {
                callId: String(item.call_id ?? ''),
                name: String(item.name ?? ''),
                args: '',
              };
            }
            break;
          }

          case 'response.function_call_arguments.delta': {
            const delta = payload.delta;
            if (pending !== null && typeof delta === 'string') {
              pending.args += delta;
            }
            break;
          }

          case 'response.output_item.done': {
            const item = payload.item as Record<string, unknown> | undefined;
            if (item?.type === 'function_call' && pending !== null) {
              yield {
                kind: 'function_call',
                callId: pending.callId,
                name: pending.name,
                arguments:
                  pending.args.length > 0 ? pending.args : String(item.arguments ?? '{}'),
              };
              pending = null;
            }
            break;
          }

          case 'response.failed':
          case 'error': {
            const error = payload.error as Record<string, unknown> | undefined;
            const message =
              (typeof error?.message === 'string' ? error.message : null) ??
              (typeof payload.message === 'string' ? payload.message : null) ??
              'Bailian reported a failed response.';
            yield { kind: 'failed', message };
            return;
          }

          default:
            break;
        }
      }
    } finally {
      clearIdle();
      controller.abort();
      options.signal?.removeEventListener('abort', forwardAbort);
    }
  }
}
