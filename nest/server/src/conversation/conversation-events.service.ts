import { Inject, Injectable, Logger } from '@nestjs/common';
import { and, asc, desc, eq, gt, lt, max, min, sql } from 'drizzle-orm';
import { Observable, Subject } from 'rxjs';
import type { Mode } from '@/agent/mode';
import { DRIZZLE_INSTANCE, type DrizzleDb } from '@/database/database.module';
import { conversationEvents } from '@/database/schema';
import type { TaskKind, TaskStatus } from '@/task/task.service';
import type { AssetView, MessageView } from './conversation.service';

/**
 * One journaled event on a conversation stream. The `type` discriminant is the
 * same name the SSE frame carries, and the whole object is what the frame's
 * `data:` holds — stored verbatim so a replayed frame is byte-equivalent to
 * the live one. (Names: src/common/sse.ts.)
 */
export type ConversationEvent =
  | { type: 'user_message'; message: MessageView }
  | { type: 'turn_started'; messageId: number; mode: Mode | null }
  | { type: 'reasoning'; messageId: number; delta: string }
  | { type: 'text'; messageId: number; delta: string }
  | {
      type: 'tool_call';
      messageId: number;
      name: string;
      arguments: Record<string, unknown>;
      status: string;
    }
  | { type: 'tool_result'; messageId: number; name: string; ok: boolean; summary: string }
  | {
      type: 'asset';
      messageId: number;
      id: number;
      kind: AssetView['kind'];
      url: string;
      mime: string;
      bytes: number;
    }
  | { type: 'turn_done'; messageId: number }
  | { type: 'turn_error'; messageId: number; message: string }
  | {
      type: 'task_updated';
      taskId: number;
      kind: TaskKind;
      status: TaskStatus;
      error: string | null;
    }
  | { type: 'message_added'; message: MessageView };

/** An event together with the journal id that becomes its SSE `id:` field. */
export interface PublishedEvent {
  seq: number;
  event: ConversationEvent;
}

/**
 * How many recent events a conversation's journal keeps. Sized above the
 * delta count of one ordinary turn (docs/verification.md §1: `high` reasoning
 * effort produced >1300 deltas), so a brief disconnect replays seamlessly and
 * only long-stale cursors fall back to `resync`.
 */
const RETAINED_EVENTS = 2000;

/**
 * How often one event may be re-inserted after losing the cursor race. Two is already
 * one more than the design expects; the limit exists so a pathological writer cannot
 * pin a delta loop in retry.
 */
const SEQ_RETRY_LIMIT = 3;

/**
 * Whether a failed insert is the UNIQUE cursor index firing, i.e. another writer took
 * the `seq` this statement computed.
 *
 * better-sqlite3 labels the error class but not the constraint, so the index name in
 * the message is what tells this apart from a broken foreign key. Retrying anything
 * else would hide a missing column behind three pointless attempts.
 */
function isSeqConflict(error: unknown): boolean {
  if (!(error instanceof Error)) {
    return false;
  }
  // Narrowed rather than asserted: `code` is better-sqlite3's own field, and `Error`
  // does not declare it.
  const code = 'code' in error && typeof error.code === 'string' ? error.code : '';
  return code.startsWith('SQLITE_CONSTRAINT') && error.message.includes('idx_events_conversation');
}

/** The message a journal row should hang off, for mid-turn catch-up lookups. */
function messageIdOf(event: ConversationEvent): number | null {
  if (event.type === 'user_message' || event.type === 'message_added') {
    return event.message.id;
  }
  if (event.type === 'task_updated') {
    return null;
  }
  return event.messageId;
}

/**
 * Durable event log + live fan-out for one conversation's stream — the single
 * source of order for everything the browser (or any future client) watches
 * (docs/roadmap.md §2.4.1).
 *
 * Before this, turn deltas lived only on the POST that opened them and
 * background events only in an in-memory Subject (`TaskEventsService`): two
 * streams, neither resumable. Now every event is inserted into
 * `conversation_events` before it is fanned out, so its autoincrement id is
 * the stream `seq`, a reconnect can replay from Last-Event-ID, and turns and
 * background jobs share one ordering.
 *
 * The fan-out is still in-process: a second instance would need a real broker
 * (unchanged limitation, documented with the single-process deployment).
 */
@Injectable()
export class ConversationEventsService {
  constructor(@Inject(DRIZZLE_INSTANCE) private readonly db: DrizzleDb) {}

  private readonly logger = new Logger(ConversationEventsService.name);

  private readonly streams = new Map<number, Subject<PublishedEvent>>();

  /**
   * Journals the event, then delivers it to every live subscriber. Both steps
   * are synchronous, so within one process the journal order and the fan-out
   * order are the same order — a subscriber cannot see id 12 before id 11.
   * Returns the journal id so publishers can stamp frames without re-reading.
   */
  publish(conversationId: number, event: ConversationEvent): PublishedEvent {
    const seq = this.append(conversationId, event);
    const published: PublishedEvent = { seq, event };
    this.streams.get(conversationId)?.next(published);
    return published;
  }

  /**
   * Writes one journal row and returns its `seq`.
   *
   * The next cursor value is computed **inside** the insert, in one statement, because
   * that is what makes it a single writer. This service used to read `MAX(seq)` and
   * then insert in a second statement — two autocommit transactions, so anything could
   * happen between them, and with `nest/server` and `rust/server` sharing one database
   * file the other stack's row landed in that gap (docs/architecture.md §13.2). Two
   * backends are online together by decision now, so the race is real: either the same
   * `seq` lands twice — invisible to every reader, because live delivery and replay
   * both filter `seq >` — or the writers contend and one frame is lost.
   *
   * A write statement takes SQLite's write lock before it runs, so the read and the
   * insert are in one critical section without any transaction plumbing, and a second
   * writer waits rather than guessing. The UNIQUE cursor index makes a duplicate `seq`
   * an error instead of silence, which is what makes this safe to run next to a build
   * that predates it: during that rollout window a conflict is possible, and it is
   * retried here. Same shape as `rust/server`'s `EventLog::append`.
   */
  private append(conversationId: number, event: ConversationEvent): number {
    const statement = sql`
      INSERT INTO conversation_events (conversation_id, seq, message_id, type, payload, created_at)
      SELECT ${conversationId}, COALESCE(MAX(seq), 0) + 1, ${messageIdOf(event)}, ${event.type},
             ${JSON.stringify(event)}, ${Date.now()}
      FROM conversation_events WHERE conversation_id = ${conversationId}
      RETURNING seq`;
    for (let attempt = 1; ; attempt += 1) {
      try {
        const row = this.db.get<{ seq: number }>(statement);
        // The statement aggregates without a GROUP BY, so it always emits exactly one
        // row and the insert always happens. Absent would mean SQLite changed the rule.
        if (row === undefined) {
          throw new Error('the journal insert took no row');
        }
        return Number(row.seq);
      } catch (error: unknown) {
        if (isSeqConflict(error) && attempt < SEQ_RETRY_LIMIT) {
          this.logger.warn(
            `journal seq collided with another writer (conversation ${conversationId}, attempt ${attempt}), retrying`,
          );
          continue;
        }
        throw error;
      }
    }
  }

  /** Live tail only; replay is the reader's job (see `replayAfter`). */
  subscribe(conversationId: number): Observable<PublishedEvent> {
    let subject = this.streams.get(conversationId);
    if (subject === undefined) {
      subject = new Subject<PublishedEvent>();
      this.streams.set(conversationId, subject);
    }
    const current = subject;

    // Drop the subject once the last listener leaves, so idle conversations do
    // not accumulate map entries for the lifetime of the process.
    return new Observable<PublishedEvent>((subscriber) => {
      const subscription = current.subscribe(subscriber);
      return () => {
        subscription.unsubscribe();
        if (current.observers.length === 0 && this.streams.get(conversationId) === current) {
          this.streams.delete(conversationId);
        }
      };
    });
  }

  /** Conversation seq of the most recent event, 0 when the conversation has none. */
  latestSeq(conversationId: number): number {
    const row = this.db
      .select({ top: max(conversationEvents.seq) })
      .from(conversationEvents)
      .where(eq(conversationEvents.conversationId, conversationId))
      .get();
    return row?.top ?? 0;
  }

  /** Oldest retained seq, or null when nothing is retained (never published). */
  oldestSeq(conversationId: number): number | null {
    const row = this.db
      .select({ bottom: min(conversationEvents.seq) })
      .from(conversationEvents)
      .where(eq(conversationEvents.conversationId, conversationId))
      .get();
    return row?.bottom ?? null;
  }

  /**
   * Events a cursor missed, oldest first. A cursor strictly below the retained
   * window is a gap the caller must report as `resync`, not pad with a partial
   * replay — this method itself does not detect that; see `hasGap`.
   */
  replayAfter(conversationId: number, seq: number): PublishedEvent[] {
    return this.db
      .select()
      .from(conversationEvents)
      .where(
        and(eq(conversationEvents.conversationId, conversationId), gt(conversationEvents.seq, seq)),
      )
      .orderBy(asc(conversationEvents.seq))
      .all()
      .map((row) => ({
        seq: row.seq,
        // Server-written JSON of our own event union; parse cannot fail in any
        // way the reader could act on, and a corrupt row would fail the same
        // way at publish time.
        event: JSON.parse(row.payload) as ConversationEvent,
      }));
  }

  /**
   * Whether a cursor cannot be honored: it precedes the retained window. Equal
   * to the oldest id minus one, or newer, is replayable — the cursor itself
   * need not exist, only everything after it.
   */
  hasGap(conversationId: number, cursor: number): boolean {
    const oldest = this.oldestSeq(conversationId);
    return oldest !== null && cursor < oldest - 1;
  }

  /**
   * Catch-up starting point for a conversation with a still-unfinished
   * assistant row: the event just before that turn's first journal entry, so a
   * fresh connection replays the live turn from its beginning. Falls back to
   * the newest seq when the turn predates the retained window (gap → resync).
   */
  seqBeforeTurn(conversationId: number, messageId: number): number {
    const row = this.db
      .select({ seq: conversationEvents.seq })
      .from(conversationEvents)
      .where(
        and(
          eq(conversationEvents.conversationId, conversationId),
          eq(conversationEvents.messageId, messageId),
        ),
      )
      .orderBy(asc(conversationEvents.seq))
      .get();
    if (row === undefined) {
      return this.latestSeq(conversationId);
    }
    // A turn's user_message precedes turn_started by one row in the same tick;
    // starting one before the turn's first row also replays the user echo.
    return row.seq - 1;
  }

  /**
   * Drops everything older than the retained window. Called when a turn ends,
   * so a conversation's journal stays small while a live turn's events — the
   * newest ones — are never pruned out from under a reconnecting client.
   */
  prune(conversationId: number): void {
    const kept = this.db
      .select({ seq: conversationEvents.seq })
      .from(conversationEvents)
      .where(eq(conversationEvents.conversationId, conversationId))
      .orderBy(desc(conversationEvents.seq))
      .limit(RETAINED_EVENTS)
      .all();
    const boundary = kept[kept.length - 1];
    if (kept.length === RETAINED_EVENTS && boundary !== undefined) {
      this.db
        .delete(conversationEvents)
        .where(
          and(
            eq(conversationEvents.conversationId, conversationId),
            lt(conversationEvents.seq, boundary.seq),
          ),
        )
        .run();
    }
  }
}
