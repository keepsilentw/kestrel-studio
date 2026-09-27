import { Test } from '@nestjs/testing';
import Database from 'better-sqlite3';
import { drizzle } from 'drizzle-orm/better-sqlite3';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { ConversationService } from '@/conversation/conversation.service';
import { applySchema, DRIZZLE_INSTANCE } from '@/database/database.module';
import * as schema from '@/database/schema';
import type { GenerationTask } from '@/database/schema';
import { TaskEventsService, type ConversationEvent } from '@/task/task-events.service';
import { TaskService, type NewTask } from '@/task/task.service';

/**
 * Real in-memory SQLite with the production DDL, as in the conversation tests.
 * `generation_tasks` rows resume across a restart, so the status columns and
 * the active/terminal split are the actual contract under test.
 */
interface Harness {
  tasks: TaskService;
  conversations: ConversationService;
  seedConversation(): number;
  received(conversationId: number): ConversationEvent[];
  raw: Database.Database;
}

let harness: Harness;

const newTask = (conversationId: number, overrides: Partial<NewTask> = {}): NewTask => ({
  conversationId,
  messageId: null,
  kind: 'video',
  providerTaskId: 'provider-1',
  model: 'happyhorse-1.1-t2v',
  prompt: '一只红隼掠过水面',
  params: { size: '1280*720', duration: 5 },
  ...overrides,
});

async function buildHarness(): Promise<Harness> {
  const connection = new Database(':memory:');
  applySchema(connection);
  const db = drizzle(connection, { schema });

  const moduleRef = await Test.createTestingModule({
    providers: [
      TaskService,
      TaskEventsService,
      ConversationService,
      { provide: DRIZZLE_INSTANCE, useValue: db },
    ],
  }).compile();

  const tasks = moduleRef.get(TaskService);
  const conversations = moduleRef.get(ConversationService);
  const events = moduleRef.get(TaskEventsService);

  // A fresh user per call: usernames are unique, and tests that need two
  // conversations want them owned by two owners anyway.
  let seeded = 0;

  return {
    tasks,
    conversations,
    raw: connection,
    seedConversation(): number {
      seeded += 1;
      const user = db
        .insert(schema.users)
        .values({ username: `u${seeded}`, passwordHash: 'x', role: 'user', createdAt: new Date() })
        .run();
      return conversations.create(Number(user.lastInsertRowid), 'q');
    },
    received(conversationId: number): ConversationEvent[] {
      const collected: ConversationEvent[] = [];
      events.subscribe(conversationId).subscribe((event) => collected.push(event));
      return collected;
    },
  };
}

/** Narrows rather than asserting: the row must exist because the test made it. */
function taskRow(id: number): GenerationTask {
  const row = harness.tasks.findById(id);
  if (row === null) {
    throw new Error(`task ${id} not found`);
  }
  return row;
}

beforeEach(async () => {
  harness = await buildHarness();
});

afterEach(() => {
  harness.raw.close();
});

describe('TaskService.create', () => {
  it('落库为 queued，尝试次数从 0 开始', () => {
    const conversationId = harness.seedConversation();
    const id = harness.tasks.create(newTask(conversationId));

    const row = harness.tasks.findById(id);
    expect(row).toMatchObject({
      status: 'queued',
      attempts: 0,
      error: null,
      assetId: null,
      resultMessageId: null,
      finishedAt: null,
      kind: 'video',
      providerTaskId: 'provider-1',
    });
  });

  it('params 以 JSON 文本存列', () => {
    const conversationId = harness.seedConversation();
    const id = harness.tasks.create(newTask(conversationId));

    expect(JSON.parse(taskRow(id).params ?? '')).toEqual({
      size: '1280*720',
      duration: 5,
    });
  });

  it('创建时立刻广播一次 task_updated，不等第一次轮询', () => {
    // The next sweep is up to POLL_INTERVAL_MS away; the user who just submitted
    // should see the job appear at once.
    const conversationId = harness.seedConversation();
    const listener = harness.received(conversationId);
    const id = harness.tasks.create(newTask(conversationId));

    expect(listener).toEqual([
      { type: 'task_updated', taskId: id, kind: 'video', status: 'queued', error: null },
    ]);
  });

  it('findById 对不存在的 id 返回 null', () => {
    expect(harness.tasks.findById(9999)).toBeNull();
  });
});

describe('TaskService — 活跃与终态的分界', () => {
  it('active 只返回 queued / running', () => {
    const conversationId = harness.seedConversation();
    const queued = harness.tasks.create(newTask(conversationId, { providerTaskId: 'a' }));
    const running = harness.tasks.create(newTask(conversationId, { providerTaskId: 'b' }));
    const done = harness.tasks.create(newTask(conversationId, { providerTaskId: 'c' }));
    const failed = harness.tasks.create(newTask(conversationId, { providerTaskId: 'd' }));

    harness.tasks.markProgress(running, 'running', 1);
    harness.tasks.finish(done, { status: 'succeeded', assetId: 5 });
    harness.tasks.finish(failed, { status: 'failed', error: 'boom' });

    expect(harness.tasks.active().map((task) => task.id)).toEqual([queued, running]);
  });

  it('active 按 id 升序，先提交的先推进', () => {
    const conversationId = harness.seedConversation();
    const first = harness.tasks.create(newTask(conversationId, { providerTaskId: 'a' }));
    const second = harness.tasks.create(newTask(conversationId, { providerTaskId: 'b' }));

    expect(harness.tasks.active().map((task) => task.id)).toEqual([first, second]);
  });

  it('active 尊重 limit', () => {
    const conversationId = harness.seedConversation();
    for (const id of ['a', 'b', 'c']) {
      harness.tasks.create(newTask(conversationId, { providerTaskId: id }));
    }
    expect(harness.tasks.active(2)).toHaveLength(2);
  });

  it('activeByConversation 只看本会话，且只算活跃的', () => {
    const mine = harness.seedConversation();
    const other = harness.seedConversation();
    const active = harness.tasks.create(newTask(mine, { providerTaskId: 'a' }));
    const finished = harness.tasks.create(newTask(mine, { providerTaskId: 'b' }));
    harness.tasks.create(newTask(other, { providerTaskId: 'c' }));
    harness.tasks.finish(finished, { status: 'failed', error: 'boom' });

    expect(harness.tasks.activeByConversation(mine).map((task) => task.id)).toEqual([active]);
  });

  it('byConversation 包含终态任务', () => {
    const mine = harness.seedConversation();
    const other = harness.seedConversation();
    const finished = harness.tasks.create(newTask(mine, { providerTaskId: 'b' }));
    harness.tasks.create(newTask(other, { providerTaskId: 'c' }));
    harness.tasks.finish(finished, { status: 'succeeded', assetId: 1 });

    expect(harness.tasks.byConversation(mine).map((task) => task.id)).toEqual([finished]);
  });
});

describe('TaskService — 状态写入', () => {
  it('markProgress 更新状态与尝试次数', () => {
    const conversationId = harness.seedConversation();
    const id = harness.tasks.create(newTask(conversationId));

    harness.tasks.markProgress(id, 'running', 2);

    expect(harness.tasks.findById(id)).toMatchObject({ status: 'running', attempts: 2 });
  });

  it('markAttemptFailed 记录错误但不改变状态', () => {
    // A failed mirror attempt is recoverable: the provider already rendered the
    // video, so the task stays active and the worker retries.
    const conversationId = harness.seedConversation();
    const id = harness.tasks.create(newTask(conversationId));
    harness.tasks.markProgress(id, 'running', 1);

    harness.tasks.markAttemptFailed(id, 2, 'mirror failed');

    expect(harness.tasks.findById(id)).toMatchObject({
      status: 'running',
      attempts: 2,
      error: 'mirror failed',
    });
    expect(harness.tasks.active().map((task) => task.id)).toEqual([id]);
  });

  it('finish 写入终态、资产与结果消息，并盖上完成时间', () => {
    const conversationId = harness.seedConversation();
    const id = harness.tasks.create(newTask(conversationId));

    harness.tasks.finish(id, { status: 'succeeded', assetId: 7, resultMessageId: 8 });

    const row = taskRow(id);
    expect(row).toMatchObject({ status: 'succeeded', assetId: 7, resultMessageId: 8, error: null });
    expect(row.finishedAt).not.toBeNull();
  });

  it('finish 会清掉之前记下的镜像错误', () => {
    // Otherwise a task that eventually succeeded would still carry the
    // transient error from an earlier mirror attempt into the UI.
    const conversationId = harness.seedConversation();
    const id = harness.tasks.create(newTask(conversationId));
    harness.tasks.markAttemptFailed(id, 1, 'mirror failed');

    harness.tasks.finish(id, { status: 'succeeded', assetId: 7 });

    expect(taskRow(id).error).toBeNull();
  });

  it('finish 成失败时记录原因并离开活跃集合', () => {
    const conversationId = harness.seedConversation();
    const id = harness.tasks.create(newTask(conversationId));

    harness.tasks.finish(id, { status: 'failed', error: '渲染超时' });

    expect(harness.tasks.findById(id)).toMatchObject({ status: 'failed', error: '渲染超时' });
    expect(harness.tasks.active()).toEqual([]);
  });
});
