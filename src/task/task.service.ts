import { Inject, Injectable } from '@nestjs/common';
import { and, asc, eq, inArray } from 'drizzle-orm';
import { DRIZZLE_INSTANCE, type DrizzleDb } from '@/database/database.module';
import { generationTasks, type GenerationTask } from '@/database/schema';
import { TaskEventsService } from './task-events.service';

/** Mirrors the provider's own vocabulary so a status never needs translating twice. */
export type TaskStatus = 'queued' | 'running' | 'succeeded' | 'failed';

export type TaskKind = 'image' | 'video';

/** A task in one of these states is still owned by the background worker. */
export const ACTIVE_STATUSES: readonly TaskStatus[] = ['queued', 'running'];

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
    private readonly events: TaskEventsService,
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
