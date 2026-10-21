import { Injectable, Logger } from '@nestjs/common';
import { BailianResponsesClient, type ResponsesInputItem } from '@/bailian/responses-client';
import { parseToolArguments } from '@/common/args';
import { describeError } from '@/common/errors';
import {
  ConversationEventsService,
  type ConversationEvent,
} from '@/conversation/conversation-events.service';
import {
  ConversationService,
  assetUrl,
  type ToolCallRecord,
} from '@/conversation/conversation.service';
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
    private readonly events: ConversationEventsService,
  ) {}

  /**
   * Executes one agent turn for a conversation. The rows the turn writes into
   * (the user message, the assistant placeholder) already exist: the caller
   * (`TurnRunner`) created them synchronously before this returns control, so
   * submitting and running are decoupled from any HTTP connection — which is
   * what lets the turn outlive a dropped client (docs/roadmap.md §2.4.1).
   *
   * Everything the browser sees goes through the conversation's event journal,
   * not a response stream: each event gets a `seq` and is replayable after a
   * reconnect. Never rejects — the journal outcome is `turn_done` or
   * `turn_error`, and the placeholder is always finalized.
   */
  async run(options: {
    conversationId: number;
    messageId: number;
    mode: Mode;
  }): Promise<void> {
    const { conversationId, messageId, mode } = options;

    const items: ResponsesInputItem[] = this.conversations.buildInputItems(conversationId);
    const toolRecords: ToolCallRecord[] = [];
    let reasoning = '';
    let answer = '';
    let failure: string | null = null;

    const emit = (event: ConversationEvent): void => {
      this.events.publish(conversationId, event);
    };

    try {
      for (let round = 0; round < MAX_TOOL_ROUNDS; round += 1) {
        const calls: ParsedCall[] = [];

        for await (const event of this.llm.stream({
          input: items,
          tools: this.tools.specsFor(mode),
          instructions: buildInstructions(mode),
        })) {
          switch (event.kind) {
            case 'reasoning':
              reasoning += event.delta;
              emit({ type: 'reasoning', messageId, delta: event.delta });
              break;
            case 'text':
              answer += event.delta;
              emit({ type: 'text', messageId, delta: event.delta });
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
          emit({
            type: 'tool_call',
            messageId,
            name: call.name,
            arguments: args,
            status: 'running',
          });

          const outcome = await this.tools.execute(call.name, args, {
            conversationId,
            messageId,
          });

          const assetIds: number[] = [];
          for (const asset of outcome.assets) {
            const assetId = this.conversations.addAsset(messageId, asset);
            assetIds.push(assetId);
            emit({
              type: 'asset',
              messageId,
              id: assetId,
              kind: asset.kind,
              url: assetUrl(assetId),
              mime: asset.mime,
              bytes: asset.bytes,
            });
          }

          const summary = describeOutcome(outcome, assetIds);
          emit({ type: 'tool_result', messageId, name: call.name, ok: outcome.ok, summary });
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
    } catch (error) {
      failure = describeError(error);
      this.logger.warn(`Agent turn failed: ${failure}`);
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

      // Journaled after the rows are durable: a client that reconnects past
      // this event and resyncs reads the finalized content, never the
      // placeholder. `done` used to ride the same POST connection as the
      // finalization writes; journaling reorders them, so this makes it whole.
      if (failure === null) {
        emit({ type: 'turn_done', messageId });
      } else {
        emit({ type: 'turn_error', messageId, message: failure });
      }
    }
  }
}
