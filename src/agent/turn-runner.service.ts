import { Injectable, Logger, type OnApplicationBootstrap } from '@nestjs/common';
import { describeError } from '@/common/errors';
import { ConversationEventsService } from '@/conversation/conversation-events.service';
import { ConversationService } from '@/conversation/conversation.service';
import type { Mode } from './mode';
import { AgentService } from './agent.service';

/**
 * Owns the turns running in this process: one per conversation, detached from
 * the request that submitted it.
 *
 * `POST /api/v1/conversations/:id/turns` answers 202 as soon as the rows exist
 * and the run is in flight. Two decisions ride on that detachment:
 *
 * - A dropped connection no longer aborts the turn. Closing the page mid-turn
 *   used to cancel the paid generation with the request; now the turn finishes
 *   server-side and a reconnect catches up on the stream. That is the whole
 *   point of phase 2 (docs/roadmap.md §2.4.1).
 * - One running turn per conversation, answered with 409 on the second
 *   attempt: the old guard was the single POST connection; with send and
 *   subscribe split, nothing else stops a double submit from billing twice.
 */
@Injectable()
export class TurnRunner implements OnApplicationBootstrap {
  private readonly logger = new Logger(TurnRunner.name);
  /** conversationId -> the assistant message of its running turn. */
  private readonly running = new Map<number, number>();

  constructor(
    private readonly agent: AgentService,
    private readonly conversations: ConversationService,
    private readonly events: ConversationEventsService,
  ) {}

  /**
   * A hard kill (restart, crash) stops a running turn with its placeholder
   * empty. Ending those rows here — before any turn can read history — keeps
   * the orphaned-user-message hazard `AgentService`'s finally guards against
   * from surviving a process boundary.
   */
  onApplicationBootstrap(): void {
    const abandoned = this.conversations.abandonStalePlaceholders();
    if (abandoned > 0) {
      this.logger.log(
        `Finalized ${abandoned} assistant row(s) left unfinished by a previous process`,
      );
    }
  }

  isRunning(conversationId: number): boolean {
    return this.running.has(conversationId);
  }

  /**
   * Where a stream connection with no client cursor starts.
   *
   * A conversation whose turn is still running gets its cursor pushed back to
   * just before that turn's first journal event — SSR deliberately does not
   * paint an unfinished assistant row, so the replay must carry the turn from
   * its beginning. Otherwise the cursor is the newest event: everything older
   * is already in the rows the page was painted from.
   */
  catchUpCursor(conversationId: number): number {
    const unfinished = this.conversations.unfinishedAssistant(conversationId);
    if (unfinished !== null && this.isRunning(conversationId)) {
      return this.events.seqBeforeTurn(conversationId, unfinished);
    }
    return this.events.latestSeq(conversationId);
  }

  /**
   * Starts a turn and returns its assistant message id — the turn's identity,
   * which the stream also calls it by (`turn_started`, deltas, assets).
   * Returns null when a turn is already running for that conversation.
   *
   * Everything durable happens synchronously before returning: the submit
   * endpoint must not answer 202 and then have the turn's rows appear
   * "later", and the journal order user_message -> turn_started -> deltas is
   * what replaying clients depend on.
   */
  start(options: { conversationId: number; prompt: string; mode: Mode }): number | null {
    const { conversationId, prompt, mode } = options;
    if (this.running.has(conversationId)) {
      return null;
    }

    const userMessageId = this.conversations.appendUserMessage(conversationId, prompt, mode);
    const messageId = this.conversations.createAssistantPlaceholder(conversationId, mode);
    this.running.set(conversationId, messageId);

    const userView = this.conversations.messageView(userMessageId);
    if (userView !== null) {
      this.events.publish(conversationId, { type: 'user_message', message: userView });
    }
    this.events.publish(conversationId, { type: 'turn_started', messageId, mode });

    this.agent
      .run({ conversationId, messageId, mode })
      .catch((error: unknown) => {
        // AgentService never rejects in practice; if it somehow did (a journal
        // insert throwing inside its finally), still release the slot and say
        // so, rather than leaving the conversation locked with an empty
        // placeholder for the next boot's recovery.
        this.logger.warn(
          `Turn ${messageId} escaped its own failure handling: ${describeError(error)}`,
        );
        try {
          this.conversations.finalizeAssistant(messageId, {
            content: `本轮处理失败：${describeError(error)}`,
            reasoning: null,
            toolCalls: null,
          });
        } catch {
          // Nothing left to do: the boot recovery will end the row.
        }
      })
      .finally(() => {
        this.running.delete(conversationId);
        this.events.prune(conversationId);
      });

    return messageId;
  }
}
