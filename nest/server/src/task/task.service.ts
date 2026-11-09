import { Inject, Injectable } from '@nestjs/common';
import { and, asc, eq, inArray, isNull, lt, or } from 'drizzle-orm';
import { WRITER_ID } from '@/common/writer';
import { ConversationEventsService } from '@/conversation/conversation-events.service';
import { DRIZZLE_INSTANCE, type DrizzleDb } from '@/database/database.module';
import { generationTasks, type GenerationTask } from '@/database/schema';

/** Mirrors the provider's own vocabulary so a status never needs translating twice. */
export type TaskStatus = 'queued' | 'running' | 'succeeded' | 'failed';

export type TaskKind = 'image' | 'video';

/** A task in one of these states is still owned by the background worker. */
export const ACTIVE_STATUSES: readonly TaskStatus[] = ['queued', 'running'];

/**
 * How long a claim on a render holds against the *other* backend. A sweep renews its
 * own claim every tick regardless of this window, so the value only decides how long a
 * task stays with a stack that stopped advancing it — one crashed poll plus the longest
 * mirror download. Five minutes is comfortably inside the 20-minute age limit the
 * worker already enforces, so a lease that is never renewed still ends in a terminal
 * state rather than an orphan. `rust/server/src/task/mod.rs` carries the same number:
 * both stacks read and write one table.
 */
export const CLAIM_LEASE_MS = 5 * 60 * 1000;

export interface NewTask {
  conversationId: number;
  messageId: number | null;
  kind: TaskKind;
  providerTaskId: string;
  model: string;
  prompt: string;
  params: Record<string, unknown>;
}

@Injectable()
export class TaskService {
  constructor(
    @Inject(DRIZZLE_INSTANCE) private readonly db: DrizzleDb,
    private readonly events: ConversationEventsService,
  ) {}

  create(task: NewTask): number {
    const now = new Date();
    const result = this.db
      .insert(generationTasks)
      .values({
        conversationId: task.conversationId,
        messageId: task.messageId,
        kind: task.kind,
        providerTaskId: task.providerTaskId,
        model: task.model,
        prompt: task.prompt,
        params: JSON.stringify(task.params),
        status: 'queued',
        attempts: 0,
        error: null,
        assetId: null,
        resultMessageId: null,
        createdAt: now,
        updatedAt: now,
        finishedAt: null,
      })
      .run();
    const id = Number(result.lastInsertRowid);

    // Announce immediately, not on the first poll: the next sweep is up to
    // `POLL_INTERVAL_MS` away, and the user who just submitted should see the
    // job appear at once rather than after the turn has already gone quiet.
    this.events.publish(task.conversationId, {
      type: 'task_updated',
      taskId: id,
      kind: task.kind,
      status: 'queued',
      error: null,
    });
    return id;
  }

  findById(id: number): GenerationTask | null {
    const row = this.db
      .select()
      .from(generationTasks)
      .where(eq(generationTasks.id, id))
      .get();
    return row ?? null;
  }

  /** Every task the worker still has to advance, oldest first. */
  active(limit = 20): GenerationTask[] {
    return this.db
      .select()
      .from(generationTasks)
      .where(inArray(generationTasks.status, [...ACTIVE_STATUSES]))
      .orderBy(asc(generationTasks.id))
      .limit(limit)
      .all();
  }

  activeByConversation(conversationId: number): GenerationTask[] {
    return this.db
      .select()
      .from(generationTasks)
      .where(
        and(
          eq(generationTasks.conversationId, conversationId),
          inArray(generationTasks.status, [...ACTIVE_STATUSES]),
        ),
      )
      .orderBy(asc(generationTasks.id))
      .all();
  }

  byConversation(conversationId: number): GenerationTask[] {
    return this.db
      .select()
      .from(generationTasks)
      .where(eq(generationTasks.conversationId, conversationId))
      .orderBy(asc(generationTasks.id))
      .all();
  }

  markProgress(id: number, status: TaskStatus, attempts: number): void {
    this.db
      .update(generationTasks)
      .set({ status, attempts, updatedAt: new Date() })
      .where(eq(generationTasks.id, id))
      .run();
  }

  /**
   * Takes the lease on one non-terminal task and reports whether it is ours now.
   *
   * This single statement is the whole guard. A worker that reads the queue and then
   * acts — what both backends did until 2026-11-09 — polls the provider twice, mirrors
   * the same video twice and journals the result twice, because the non-terminal window
   * of a render runs in minutes (docs/architecture.md §13.2). SQLite serializes
   * writers, so exactly one stack sees a changed row count.
   *
   * A row is claimable three ways: nobody holds it, this stack holds it (the next tick
   * renews its own lease, and so does the process that booted after a crash, which must
   * resume the renders its predecessor was polling), or a foreign lease is older than
   * `CLAIM_LEASE_MS`.
   *
   * `attempts` is deliberately not touched here: it counts provider failures and drives
   * `MAX_MIRROR_ATTEMPTS`, so a lease renewed every eight seconds would run that counter
   * away long before anything had failed.
   */
  claim(id: number, writer: string = WRITER_ID): boolean {
    const now = new Date();
    const result = this.db
      .update(generationTasks)
      .set({ claimedBy: writer, claimedAt: now, updatedAt: now })
      .where(
        and(
          eq(generationTasks.id, id),
          inArray(generationTasks.status, [...ACTIVE_STATUSES]),
          or(
            isNull(generationTasks.claimedBy),
            eq(generationTasks.claimedBy, writer),
            // A row written before the lease columns existed is unheld, whatever the
            // other disjuncts say about it.
            isNull(generationTasks.claimedAt),
            lt(generationTasks.claimedAt, new Date(now.getTime() - CLAIM_LEASE_MS)),
          ),
        ),
      )
      .run();
    return result.changes === 1;
  }

  /** The claim under this stack's own identity, which is what a worker wants. */
  claimAsWriter(id: number): boolean {
    return this.claim(id);
  }

  /** Records a failed mirror attempt without giving up on the task. */
  markAttemptFailed(id: number, attempts: number, error: string): void {
    this.db
      .update(generationTasks)
      .set({ attempts, error, updatedAt: new Date() })
      .where(eq(generationTasks.id, id))
      .run();
  }

  finish(
    id: number,
    patch: { status: TaskStatus; assetId?: number; resultMessageId?: number; error?: string },
  ): void {
    const now = new Date();
    this.db
      .update(generationTasks)
      .set({
        status: patch.status,
        assetId: patch.assetId ?? null,
        resultMessageId: patch.resultMessageId ?? null,
        error: patch.error ?? null,
        updatedAt: now,
        finishedAt: now,
      })
      .where(eq(generationTasks.id, id))
      .run();
  }
}
