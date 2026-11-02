import { Injectable, Logger } from '@nestjs/common';
import { parseSseStream } from '@/common/sse-parser';
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

@Injectable()
export class BailianResponsesClient {
  private readonly logger = new Logger(BailianResponsesClient.name);

  async *stream(options: StreamOptions): AsyncGenerator<StreamEvent> {
    const { bailian } = loadConfig();
    const response = await fetch(`${bailian.baseUrl}/compatible-mode/v1/responses`, {
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
        ...(options.instructions !== undefined ? { instructions: options.instructions } : {}),
      }),
      ...(options.signal !== undefined ? { signal: options.signal } : {}),
    });

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

    for await (const frame of parseSseStream(response.body)) {
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
              arguments: pending.args.length > 0 ? pending.args : String(item.arguments ?? '{}'),
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
  }
}
