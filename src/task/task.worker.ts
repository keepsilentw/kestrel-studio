import { Injectable, Logger, type OnModuleDestroy, type OnModuleInit } from '@nestjs/common';
import { ConversationService } from '@/conversation/conversation.service';
import type { GenerationTask } from '@/database/schema';
import { MediaService, type VideoTaskSnapshot } from '@/media/media.service';
import { TaskEventsService } from './task-events.service';
import { TaskService, type TaskStatus } from './task.service';

/** Provider renders are minutes long; polling faster only burns quota. */
const POLL_INTERVAL_MS = 8_000;

/** Give up on a task the provider never resolves. */
const MAX_TASK_AGE_MS = 20 * 60 * 1000;

/**
 * The provider URL is short-lived, so a failed mirror is retried on the next
 * ticks rather than dropping the render on the floor.
 */
const MAX_MIRROR_ATTEMPTS = 3;

const RESULT_MODE = 'video';

function describe(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/** The status column is free-form text; narrow it before routing on it. */
function asStatus(raw: string): TaskStatus {
  return raw === 'running' || raw === 'succeeded' || raw === 'failed' ? raw : 'queued';
}

@Injectable()
export class TaskWorker implements OnModuleInit, OnModuleDestroy {
  private readonly logger = new Logger(TaskWorker.name);
  private timer: NodeJS.Timeout | null = null;
  /** Guards against overlapping sweeps when a poll outlives the interval. */
  private sweeping = false;

  constructor(
    private readonly tasks: TaskService,
    private readonly media: MediaService,
    private readonly conversations: ConversationService,
    private readonly events: TaskEventsService,
  ) {}

  onModuleInit(): void {
    this.timer = setInterval(() => void this.sweep(), POLL_INTERVAL_MS);
  }

  onModuleDestroy(): void {
    if (this.timer !== null) {
      clearInterval(this.timer);
      this.timer = null;
    }
  }

  /**
   * Advances every non-terminal task by one poll. Tasks live in the database, so
   * a restart resumes them on the next sweep instead of losing them.
   *
   * Public so it can be driven directly: the interval is a thin wrapper around
   * it, and testing through the timer would make every assertion a race.
   */
  async sweep(): Promise<void> {
    if (this.sweeping) {
      return;
    }
    this.sweeping = true;
    try {
      const pending = this.tasks.active();
      for (const task of pending) {
        try {
          await this.advance(task);
        } catch (error) {
          // Per task, not per sweep. Provider errors are handled inside
          // advance(), so anything arriving here is an unexpected write or
          // plumbing failure — and one such task must not stop the rest of the
          // queue, or a task that always fails would starve every task behind
          // it until someone notices.
          this.logger.warn(`Task ${task.id} could not be advanced: ${describe(error)}`);
        }
      }
    } catch (error) {
      // Enumerating the queue failed: nothing to iterate. Kept so the error
      // does not escape as an unhandled rejection from the interval callback.
      this.logger.warn(`Task sweep failed: ${describe(error)}`);
    } finally {
      this.sweeping = false;
    }
  }

  private async advance(task: GenerationTask): Promise<void> {
    if (Date.now() - task.createdAt.getTime() > MAX_TASK_AGE_MS) {
      await this.fail(task, `任务超时（超过 ${Math.round(MAX_TASK_AGE_MS / 60000)} 分钟未完成）`);
      return;
    }

    let snapshot: VideoTaskSnapshot;
    try {
      snapshot = await this.media.queryVideoTask(task.providerTaskId);
    } catch (error) {
      // Transient failures are expected across a multi-minute render: count them
      // and keep polling until the age limit trips.
      const attempts = task.attempts + 1;
      const reason = describe(error);
      this.tasks.markAttemptFailed(task.id, attempts, reason);
      this.publish(task, asStatus(task.status), reason);
      return;
    }

    if (snapshot.state === 'pending' || snapshot.state === 'running') {
      const status: TaskStatus = snapshot.state === 'running' ? 'running' : 'queued';
      this.tasks.markProgress(task.id, status, task.attempts);
      this.publish(task, status, null);
      return;
    }

    if (snapshot.state === 'failed' || snapshot.videoUrl === null) {
      await this.fail(task, snapshot.message ?? `供应商返回状态 ${snapshot.rawStatus}`);
      return;
    }

    await this.mirror(task, snapshot.videoUrl);
  }

  /**
   * Downloads the render at the moment the provider reports success. This is the
   * only window in which the URL works — it is not a durable reference.
   */
  private async mirror(task: GenerationTask, videoUrl: string): Promise<void> {
    let stored;
    try {
      stored = await this.media.store('video', videoUrl);
    } catch (error) {
      const attempts = task.attempts + 1;
      const reason = describe(error);
      if (attempts < MAX_MIRROR_ATTEMPTS) {
        this.tasks.markAttemptFailed(task.id, attempts, reason);
        this.publish(task, asStatus(task.status), reason);
        return;
      }
      await this.fail(task, `结果下载失败：${reason}`);
      return;
    }

    const messageId = this.conversations.appendAssistantMessage(
      task.conversationId,
      `视频生成完成。\n\n提示词：${task.prompt}`,
      RESULT_MODE,
    );
    const assetId = this.conversations.addAsset(messageId, stored);
    this.tasks.finish(task.id, { status: 'succeeded', assetId, resultMessageId: messageId });
    this.publish(task, 'succeeded', null);
    this.publishMessage(task.conversationId, messageId);
  }

  /**
   * The failure is written into the conversation rather than only pushed: a user
   * who was offline when it happened must still find out why.
   */
  private async fail(task: GenerationTask, reason: string): Promise<void> {
    const messageId = this.conversations.appendAssistantMessage(
      task.conversationId,
      `视频生成失败：${reason}\n\n提示词：${task.prompt}`,
      RESULT_MODE,
    );
    this.tasks.finish(task.id, {
      status: 'failed',
      error: reason,
      resultMessageId: messageId,
    });
    this.publish(task, 'failed', reason);
    this.publishMessage(task.conversationId, messageId);
  }

  private publish(task: GenerationTask, status: TaskStatus, error: string | null): void {
    this.events.publish(task.conversationId, {
      type: 'task_updated',
      taskId: task.id,
      kind: task.kind,
      status,
      error,
    });
  }

  private publishMessage(conversationId: number, messageId: number): void {
    const view = this.conversations.messageView(messageId);
    if (view !== null) {
      this.events.publish(conversationId, { type: 'message_added', message: view });
    }
  }
}
