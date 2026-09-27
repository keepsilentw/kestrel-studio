import { Injectable, Logger } from '@nestjs/common';
import { BailianResponsesClient, type ResponsesInputItem } from '@/bailian/responses-client';
import { parseToolArguments } from '@/common/args';
import { describeError } from '@/common/errors';
import type { SseWriter } from '@/common/sse';
import { ConversationService, assetUrl, type ToolCallRecord } from '@/conversation/conversation.service';
import { buildInstructions, type Mode } from './mode';
import { describeOutcome, ToolRegistry } from './tools';

/** Guards against a tool loop that never converges. */
const MAX_TOOL_ROUNDS = 5;

interface ParsedCall {
  callId: string;
  name: string;
  arguments: string;
}

@Injectable()
export class AgentService {
  private readonly logger = new Logger(AgentService.name);

  constructor(
    private readonly llm: BailianResponsesClient,
    private readonly tools: ToolRegistry,
    private readonly conversations: ConversationService,
  ) {}

  async run(options: {
    conversationId: number;
    prompt: string;
    mode: Mode;
    writer: SseWriter;
    signal?: AbortSignal;
  }): Promise<void> {
    const { conversationId, prompt, mode, writer, signal } = options;

    this.conversations.appendUserMessage(conversationId, prompt, mode);
    const messageId = this.conversations.createAssistantPlaceholder(conversationId, mode);

    const items: ResponsesInputItem[] = this.conversations.buildInputItems(conversationId);
    const toolRecords: ToolCallRecord[] = [];
    let reasoning = '';
    let answer = '';
    let failure: string | null = null;

    try {
      for (let round = 0; round < MAX_TOOL_ROUNDS; round += 1) {
        const calls: ParsedCall[] = [];

        for await (const event of this.llm.stream({
          input: items,
          tools: this.tools.specsFor(mode),
          instructions: buildInstructions(mode),
          ...(signal !== undefined ? { signal } : {}),
        })) {
          switch (event.kind) {
            case 'reasoning':
              reasoning += event.delta;
              writer.send('reasoning', { delta: event.delta });
              break;
            case 'text':
              answer += event.delta;
              writer.send('text', { delta: event.delta });
              break;
            case 'function_call':
              calls.push({
                callId: event.callId,
                name: event.name,
                arguments: event.arguments,
              });
              break;
            case 'failed':
              throw new Error(event.message);
          }
        }

        if (calls.length === 0) {
          break;
        }

        for (const call of calls) {
          const args = parseToolArguments(call.arguments);
          writer.send('tool_call', { name: call.name, arguments: args, status: 'running' });

          const outcome = await this.tools.execute(call.name, args, {
            conversationId,
            messageId,
            ...(signal !== undefined ? { signal } : {}),
          });

          const assetIds: number[] = [];
          for (const asset of outcome.assets) {
            const assetId = this.conversations.addAsset(messageId, asset);
            assetIds.push(assetId);
            writer.send('asset', {
              id: assetId,
              kind: asset.kind,
              url: assetUrl(assetId),
              mime: asset.mime,
              bytes: asset.bytes,
            });
          }

          const summary = describeOutcome(outcome, assetIds);
          writer.send('tool_result', { name: call.name, ok: outcome.ok, summary });
          toolRecords.push({ name: call.name, arguments: args, ok: outcome.ok, summary });

          items.push({
            type: 'function_call',
            call_id: call.callId,
            name: call.name,
            arguments: call.arguments,
          });
          items.push({
            type: 'function_call_output',
            call_id: call.callId,
            output: summary,
          });
        }
      }

      writer.send('done', { conversationId, messageId });
    } catch (error) {
      failure = describeError(error);
      this.logger.warn(`Agent turn failed: ${failure}`);
      writer.send('error', { message: failure });
    } finally {
      // A failed turn must still leave a reply behind. `buildInputItems` skips
      // empty messages, so an empty assistant row leaves its user message
      // orphaned — and the next turn replays two user messages back to back,
      // which makes the model re-run the failed request as if it were new.
      // Measured: that duplicated a paid video submission.
      const content =
        failure === null
          ? answer
          : [answer, `本轮处理失败：${failure}`].filter((part) => part.length > 0).join('\n\n');

      this.conversations.finalizeAssistant(messageId, {
        content,
        reasoning: reasoning.length > 0 ? reasoning : null,
        toolCalls: toolRecords.length > 0 ? toolRecords : null,
      });
      this.conversations.touch(conversationId);
      writer.close();
    }
  }
}
