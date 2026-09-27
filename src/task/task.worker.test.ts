import { Test } from '@nestjs/testing';
import Database from 'better-sqlite3';
import { drizzle } from 'drizzle-orm/better-sqlite3';
import { afterEach, beforeEach, describe, expect, it, vi, type Mock } from 'vitest';
import { ConversationService } from '@/conversation/conversation.service';
import { applySchema, DRIZZLE_INSTANCE } from '@/database/database.module';
import * as schema from '@/database/schema';
import type { GenerationTask } from '@/database/schema';
import { MediaService, type AssetKind, type StoredAsset, type VideoTaskSnapshot } from '@/media/media.service';
import { TaskEventsService, type ConversationEvent } from '@/task/task-events.service';
import { TaskService } from '@/task/task.service';
import { TaskWorker } from '@/task/task.worker';

/**
 * The worker's own policy is what is under test: the age cutoff, how many mirror
 * attempts a task gets, which failures are recoverable, and what gets written to
 * the conversation. MediaService is the injected boundary and is stubbed —
 * nothing here asserts anything about the provider's wire format.
 *
 * `sweep()` is driven directly rather than through the 8s interval, which would
 * turn every assertion into a race.
 */
const MAX_TASK_AGE_MS = 20 * 60 * 1000;
const MAX_MIRROR_ATTEMPTS = 3;

interface Harness {
  worker: TaskWorker;
  tasks: TaskService;
  conversations: ConversationService;
  queryVideoTask: Mock<(providerTaskId: string) => Promise<VideoTaskSnapshot>>;
  store: Mock<(kind: AssetKind, url: string) => Promise<StoredAsset>>;
  seedTask(options?: {
    attempts?: number;
    ageMs?: number;
  }): { taskId: number; conversationId: number; providerTaskId: string };
  eventsFor(conversationId: number): ConversationEvent[];
  raw: Database.Database;
}

let harness: Harness;

const snapshot = (over: Partial<VideoTaskSnapshot> = {}): VideoTaskSnapshot => ({
  state: 'pending',
  rawStatus: 'PENDING',
  videoUrl: null,
  message: null,
  ...over,
});

const storedVideo: StoredAsset = {
  kind: 'video',
  filePath: '/app/storage/video-1.mp4',
  sourceUrl: 'https://provider.example/expiring.mp4',
  mime: 'video/mp4',
  bytes: 4096,
};

async function buildHarness(): Promise<Harness> {
  const connection = new Database(':memory:');
  applySchema(connection);
  const db = drizzle(connection, { schema });

  const queryVideoTask = vi.fn<(providerTaskId: string) => Promise<VideoTaskSnapshot>>();
  const store = vi.fn<(kind: AssetKind, url: string) => Promise<StoredAsset>>();

  const moduleRef = await Test.createTestingModule({
    providers: [
      TaskWorker,
      TaskService,
      TaskEventsService,
      ConversationService,
      { provide: DRIZZLE_INSTANCE, useValue: db },
      { provide: MediaService, useValue: { queryVideoTask, store } },
    ],
  }).compile();

  const tasks = moduleRef.get(TaskService);
  const conversations = moduleRef.get(ConversationService);
  const events = moduleRef.get(TaskEventsService);
  let seeded = 0;

  return {
    worker: moduleRef.get(TaskWorker),
    tasks,
    conversations,
    queryVideoTask,
    store,
    raw: connection,
    seedTask(options = {}) {
      seeded += 1;
      const user = db
        .insert(schema.users)
        .values({ username: `u${seeded}`, passwordHash: 'x', role: 'user', createdAt: new Date() })
        .run();
      const conversationId = conversations.create(Number(user.lastInsertRowid), 'q');
      const providerTaskId = `provider-${seeded}`;
      const taskId = tasks.create({
        conversationId,
        messageId: null,
        kind: 'video',
        providerTaskId,
        model: 'happyhorse-1.1-t2v',
        prompt: '一只红隼掠过水面',
        params: {},
      });

      if (options.ageMs !== undefined) {
        connection
          .prepare('UPDATE generation_tasks SET created_at = ? WHERE id = ?')
          .run(Date.now() - options.ageMs, taskId);
      }
      if (options.attempts !== undefined) {
        connection
          .prepare('UPDATE generation_tasks SET attempts = ? WHERE id = ?')
          .run(options.attempts, taskId);
      }
      return { taskId, conversationId, providerTaskId };
    },
    eventsFor(conversationId: number): ConversationEvent[] {
      const collected: ConversationEvent[] = [];
      events.subscribe(conversationId).subscribe((event) => collected.push(event));
      return collected;
    },
  };
}

/** Narrows rather than asserting: the row must exist because the test made it. */
function taskRow(taskId: number): GenerationTask {
  const row = harness.tasks.findById(taskId);
  if (row === null) {
    throw new Error(`task ${taskId} not found`);
  }
  return row;
}

beforeEach(async () => {
  harness = await buildHarness();
});

afterEach(() => {
  harness.raw.close();
  vi.useRealTimers();
});

describe('TaskWorker.sweep — 进行中的任务', () => {
  it('pending 映射为 queued 并广播', async () => {
    const { taskId, conversationId } = harness.seedTask();
    harness.queryVideoTask.mockResolvedValue(snapshot({ state: 'pending' }));
    const events = harness.eventsFor(conversationId);

    await harness.worker.sweep();

    expect(harness.tasks.findById(taskId)?.status).toBe('queued');
    expect(events).toContainEqual({
      type: 'task_updated',
      taskId,
      kind: 'video',
      status: 'queued',
      error: null,
    });
  });

  it('running 映射为 running 并广播', async () => {
    const { taskId, conversationId } = harness.seedTask();
    harness.queryVideoTask.mockResolvedValue(
      snapshot({ state: 'running', rawStatus: 'RUNNING' }),
    );
    const events = harness.eventsFor(conversationId);

    await harness.worker.sweep();

    expect(harness.tasks.findById(taskId)?.status).toBe('running');
    expect(events).toContainEqual({
      type: 'task_updated',
      taskId,
      kind: 'video',
      status: 'running',
      error: null,
    });
  });

  it('进行中的任务不会追加消息', async () => {
    const { conversationId } = harness.seedTask();
    harness.queryVideoTask.mockResolvedValue(snapshot({ state: 'running' }));

    await harness.worker.sweep();

    expect(harness.conversations.views(conversationId)).toEqual([]);
  });
});

describe('TaskWorker.sweep — 成功与镜像', () => {
  it('成功时把字节镜像落盘，并把结果写成一条新消息', async () => {
    const { taskId, conversationId } = harness.seedTask();
    harness.queryVideoTask.mockResolvedValue(
      snapshot({ state: 'succeeded', rawStatus: 'SUCCEEDED', videoUrl: 'https://provider/v.mp4' }),
    );
    harness.store.mockResolvedValue(storedVideo);
    const events = harness.eventsFor(conversationId);

    await harness.worker.sweep();

    // The URL is only valid in the instant success is reported.
    expect(harness.store).toHaveBeenCalledWith('video', 'https://provider/v.mp4');

    const row = taskRow(taskId);
    expect(row.status).toBe('succeeded');
    expect(row.assetId).not.toBeNull();

    const messages = harness.conversations.views(conversationId);
    expect(messages).toHaveLength(1);
    expect(messages[0].role).toBe('assistant');
    expect(messages[0].mode).toBe('video');
    expect(messages[0].content).toContain('视频生成完成');
    expect(messages[0].content).toContain('一只红隼掠过水面');
    expect(messages[0].assets).toHaveLength(1);
    expect(row.resultMessageId).toBe(messages[0].id);

    expect(events).toContainEqual({
      type: 'task_updated',
      taskId,
      kind: 'video',
      status: 'succeeded',
      error: null,
    });
    expect(events.some((event) => event.type === 'message_added')).toBe(true);
  });

  it('成功但没有 URL 时判为失败，而不是无限轮询', async () => {
    const { taskId, conversationId } = harness.seedTask();
    harness.queryVideoTask.mockResolvedValue(
      snapshot({ state: 'succeeded', rawStatus: 'SUCCEEDED', videoUrl: null, message: '无结果' }),
    );

    await harness.worker.sweep();

    expect(harness.store).not.toHaveBeenCalled();
    expect(harness.tasks.findById(taskId)?.status).toBe('failed');
    expect(harness.conversations.views(conversationId)[0].content).toContain('无结果');
  });

  it('供应商报失败时把原因写进会话，离线用户也能看到', async () => {
    const { taskId, conversationId } = harness.seedTask();
    harness.queryVideoTask.mockResolvedValue(
      snapshot({ state: 'failed', rawStatus: 'FAILED', message: '内容不合规' }),
    );

    await harness.worker.sweep();

    expect(harness.tasks.findById(taskId)?.error).toBe('内容不合规');
    expect(harness.conversations.views(conversationId)[0].content).toContain('内容不合规');
  });

  it('供应商没给原因时退化为原始状态值', async () => {
    const { taskId } = harness.seedTask();
    harness.queryVideoTask.mockResolvedValue(
      snapshot({ state: 'failed', rawStatus: 'CANCELED', message: null }),
    );

    await harness.worker.sweep();

    expect(harness.tasks.findById(taskId)?.error).toBe('供应商返回状态 CANCELED');
  });
});

describe('TaskWorker.sweep — 可恢复的失败', () => {
  it('查询抛错时记一次尝试，任务留在活跃集合里等下一轮', async () => {
    const { taskId, conversationId } = harness.seedTask();
    harness.queryVideoTask.mockRejectedValue(new Error('socket hang up'));

    await harness.worker.sweep();

    const row = taskRow(taskId);
    expect(row.attempts).toBe(1);
    // Status is untouched: the render is still running server-side.
    expect(row.status).toBe('queued');
    expect(row.error).toBe('socket hang up');
    expect(harness.tasks.active().map((task) => task.id)).toEqual([taskId]);
    expect(harness.conversations.views(conversationId)).toEqual([]);
  });

  it('镜像失败超过上限前不放弃任务', async () => {
    const { taskId, conversationId } = harness.seedTask();
    harness.queryVideoTask.mockResolvedValue(
      snapshot({ state: 'succeeded', rawStatus: 'SUCCEEDED', videoUrl: 'https://provider/v.mp4' }),
    );
    harness.store.mockRejectedValue(new Error('连接中断'));

    await harness.worker.sweep();

    const row = taskRow(taskId);
    expect(row.attempts).toBe(1);
    expect(row.status).not.toBe('failed');
    expect(harness.tasks.active().map((task) => task.id)).toEqual([taskId]);
    expect(harness.conversations.views(conversationId)).toEqual([]);
  });

  it('镜像连续失败到上限才判失败', async () => {
    // attempts is already 2, so this attempt is the third and final one.
    const { taskId, conversationId } = harness.seedTask({ attempts: MAX_MIRROR_ATTEMPTS - 1 });
    harness.queryVideoTask.mockResolvedValue(
      snapshot({ state: 'succeeded', rawStatus: 'SUCCEEDED', videoUrl: 'https://provider/v.mp4' }),
    );
    harness.store.mockRejectedValue(new Error('连接中断'));

    await harness.worker.sweep();

    const row = taskRow(taskId);
    expect(row.status).toBe('failed');
    expect(row.error).toContain('结果下载失败');
    expect(harness.conversations.views(conversationId)[0].content).toContain('结果下载失败');
  });
});

describe('TaskWorker.sweep — 超时', () => {
  it('超过时限的任务直接判失败，且不再问供应商', async () => {
    const { taskId, conversationId } = harness.seedTask({ ageMs: MAX_TASK_AGE_MS + 60_000 });

    await harness.worker.sweep();

    // The age check runs before the poll: a task nobody will resolve must not
    // keep spending quota.
    expect(harness.queryVideoTask).not.toHaveBeenCalled();
    const row = taskRow(taskId);
    expect(row.status).toBe('failed');
    expect(row.error).toContain('超时');
    expect(row.error).toContain('20 分钟');
    expect(harness.conversations.views(conversationId)[0].content).toContain('超时');
  });

  it('刚好没到时限的任务照常推进', async () => {
    const { taskId } = harness.seedTask({ ageMs: MAX_TASK_AGE_MS - 60_000 });
    harness.queryVideoTask.mockResolvedValue(snapshot({ state: 'running' }));

    await harness.worker.sweep();

    expect(harness.queryVideoTask).toHaveBeenCalledTimes(1);
    expect(harness.tasks.findById(taskId)?.status).toBe('running');
  });
});

describe('TaskWorker.sweep — 编排', () => {
  it('没有活跃任务时不调用任何依赖', async () => {
    await expect(harness.worker.sweep()).resolves.toBeUndefined();
    expect(harness.queryVideoTask).not.toHaveBeenCalled();
  });

  it('一次 sweep 推进所有活跃任务，按提交顺序', async () => {
    const first = harness.seedTask();
    const second = harness.seedTask();
    harness.queryVideoTask.mockResolvedValue(snapshot({ state: 'running' }));

    await harness.worker.sweep();

    expect(harness.queryVideoTask.mock.calls.map((call) => call[0])).toEqual([
      first.providerTaskId,
      second.providerTaskId,
    ]);
  });

  it('已终态的任务不再被推进', async () => {
    const { taskId } = harness.seedTask();
    harness.tasks.finish(taskId, { status: 'failed', error: '早先已失败' });

    await harness.worker.sweep();

    expect(harness.queryVideoTask).not.toHaveBeenCalled();
  });

  it('重入时直接返回，不并发推进同一个任务', async () => {
    // A poll that outlives the 8s interval must not start a second pass over
    // the same rows.
    const { taskId } = harness.seedTask();
    let release: (() => void) | undefined;
    harness.queryVideoTask.mockImplementation(
      () =>
        new Promise<VideoTaskSnapshot>((resolve) => {
          release = () => resolve(snapshot({ state: 'running' }));
        }),
    );

    const inFlight = harness.worker.sweep();
    await harness.worker.sweep();

    expect(harness.queryVideoTask).toHaveBeenCalledTimes(1);

    release?.();
    await inFlight;
    expect(harness.tasks.findById(taskId)?.status).toBe('running');
  });

  it('某个任务抛出意外异常时，其余任务照常推进', async () => {
    // An orphan task row stands in for the class of sudden write failure the
    // per-task catch exists for: its conversation is gone, so the message the
    // failure path writes violates the foreign key. Provider errors never reach
    // that catch — advance() handles those itself.
    harness.raw.pragma('foreign_keys = OFF');
    harness.raw
      .prepare(
        `INSERT INTO generation_tasks
           (conversation_id, message_id, kind, provider_task_id, model, prompt, params,
            status, attempts, created_at, updated_at)
         VALUES (9999, NULL, 'video', 'orphan', 'm', 'p', '{}', 'queued', 0, ?, ?)`,
      )
      .run(Date.now(), Date.now());
    harness.raw.pragma('foreign_keys = ON');

    const healthy = harness.seedTask();
    harness.queryVideoTask.mockImplementation((providerTaskId: string) =>
      Promise.resolve(
        providerTaskId === 'orphan'
          ? snapshot({ state: 'failed', rawStatus: 'FAILED', message: '写不进去' })
          : snapshot({ state: 'running' }),
      ),
    );

    // The orphan has the lower id, so it is reached first and blows up.
    await expect(harness.worker.sweep()).resolves.toBeUndefined();

    // With the catch outside the loop, everything after the orphan would be
    // skipped this tick — and had the orphan always failed, skipped forever.
    expect(taskRow(healthy.taskId).status).toBe('running');
  });

  it('一轮结束后可以再扫一轮', async () => {
    const { taskId } = harness.seedTask();
    harness.queryVideoTask.mockResolvedValue(snapshot({ state: 'running' }));

    await harness.worker.sweep();
    await harness.worker.sweep();

    expect(harness.queryVideoTask).toHaveBeenCalledTimes(2);
    expect(harness.tasks.findById(taskId)?.status).toBe('running');
  });
});

describe('TaskWorker 定时器生命周期', () => {
  it('onModuleInit 挂上轮询，onModuleDestroy 摘掉', () => {
    vi.useFakeTimers();
    expect(vi.getTimerCount()).toBe(0);

    harness.worker.onModuleInit();
    expect(vi.getTimerCount()).toBe(1);

    harness.worker.onModuleDestroy();
    expect(vi.getTimerCount()).toBe(0);
  });

  it('重复 onModuleDestroy 不抛', () => {
    vi.useFakeTimers();
    harness.worker.onModuleInit();
    harness.worker.onModuleDestroy();
    expect(() => harness.worker.onModuleDestroy()).not.toThrow();
  });
});
