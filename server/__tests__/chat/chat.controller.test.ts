import { Test } from '@nestjs/testing';
import type { NestExpressApplication } from '@nestjs/platform-express';
import Database from 'better-sqlite3';
import { drizzle } from 'drizzle-orm/better-sqlite3';
import { existsSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import type { NextFunction, Request, Response } from 'express';
import { afterAll, beforeAll, beforeEach, describe, expect, it } from 'vitest';
import { AgentService } from '@/agent/agent.service';
import type { Mode } from '@/agent/mode';
import { TurnRunner } from '@/agent/turn-runner.service';
import type { Role } from '@/auth/roles';
import { ChatController } from '@/chat/chat.controller';
import {
  ConversationEventsService,
  type ConversationEvent,
} from '@/conversation/conversation-events.service';
import { ConversationService } from '@/conversation/conversation.service';
import { applySchema, DRIZZLE_INSTANCE } from '@/database/database.module';
import * as schema from '@/database/schema';
import { TaskService } from '@/task/task.service';

/**
 * Exercised over real HTTP. The value here is the authorization layer and the
 * submit/subscribe split: every endpoint resolves the caller by hand (`canRead`
 * on the reads, `isOwnedBy` on the writes) with no interceptor behind it, and
 * the stream's replay/gap behavior is only observable from the outside.
 *
 * AgentService is the injected boundary and is stubbed — a real turn would call
 * the provider, and the agent loop is deliberately out of unit-test scope (see
 * CLAUDE.md). The stub blocks until the test releases it, because "a turn is
 * running" is itself part of the contract under test (409, catch-up cursors).
 * ConversationEventsService and TurnRunner are real: they own the journal and
 * the one-turn-per-conversation rule.
 */
const AUTH_HEADER = 'x-test-user';
const ALICE = 1;
const BOB = 2;
/** The super admin: reads everyone's normal conversations, writes only its own. */
const ROOT = 3;

const ROLE_BY_ID = new Map<number, Role>([
  [ALICE, 'user'],
  [BOB, 'user'],
  [ROOT, 'super'],
]);

interface RunCall {
  conversationId: number;
  messageId: number;
  mode: Mode;
}

let app: NestExpressApplication;
let origin: string;
let connection: Database.Database;
let conversations: ConversationService;
let tasks: TaskService;
let events: ConversationEventsService;
let runner: TurnRunner;
let runCalls: RunCall[];
let filesDir: string;

let pendingResolve: (() => void) | null = null;

/**
 * The stubbed `AgentService.run`: records the call and waits for the test.
 * Real run() journals its own outcome; releasing the stub runs the runner's
 * finally (slot released, journal pruned) without any provider anywhere.
 */
const stubAgent = {
  run: (call: RunCall): Promise<void> =>
    new Promise<void>((resolve) => {
      runCalls.push(call);
      pendingResolve = resolve;
    }),
};

/** Completes the running turn and lets the runner's finally settle. */
async function finishRunningTurn(): Promise<void> {
  const resolve = pendingResolve;
  pendingResolve = null;
  if (resolve === null) {
    return;
  }
  resolve();
  await new Promise((r) => setTimeout(r, 0));
}

async function bootstrap(): Promise<void> {
  connection = new Database(':memory:');
  applySchema(connection);
  const db = drizzle(connection, { schema });

  for (const [id, role] of ROLE_BY_ID) {
    db.insert(schema.users)
      .values({ username: `user${id}`, passwordHash: 'x', role, createdAt: new Date() })
      .run();
  }

  runCalls = [];
  const moduleRef = await Test.createTestingModule({
    controllers: [ChatController],
    providers: [
      ConversationService,
      ConversationEventsService,
      TaskService,
      TurnRunner,
      { provide: DRIZZLE_INSTANCE, useValue: db },
      { provide: AgentService, useValue: stubAgent },
    ],
  }).compile();

  conversations = moduleRef.get(ConversationService);
  tasks = moduleRef.get(TaskService);
  events = moduleRef.get(ConversationEventsService);
  runner = moduleRef.get(TurnRunner);

  app = moduleRef.createNestApplication<NestExpressApplication>();
  app.use((req: Request, _res: Response, next: NextFunction): void => {
    const header = req.headers[AUTH_HEADER];
    if (typeof header === 'string' && header.length > 0) {
      const id = Number(header);
      const role = ROLE_BY_ID.get(id);
      if (role !== undefined) {
        req.user = { id, username: `user${id}`, role };
      }
    }
    next();
  });

  await app.init();
  await app.listen(0);
  origin = await app.getUrl();
}

let previousStorageDir: string | undefined;

beforeAll(async () => {
  filesDir = mkdtempSync(join(tmpdir(), 'kestrel-chat-'));
  // Asset rows hold a file name; the download endpoint resolves it against this.
  previousStorageDir = process.env.STORAGE_DIR;
  process.env.STORAGE_DIR = filesDir;
  await bootstrap();
});

afterAll(async () => {
  await app.close();
  connection.close();
  if (previousStorageDir === undefined) {
    delete process.env.STORAGE_DIR;
  } else {
    process.env.STORAGE_DIR = previousStorageDir;
  }
  rmSync(filesDir, { recursive: true, force: true });
});

beforeEach(async () => {
  await finishRunningTurn();
  runCalls = [];
  pendingResolve = null;
  // The app instance stays up across tests, so clear the history instead of
  // rebooting it. Children first: foreign keys are on.
  connection.exec(
    'DELETE FROM conversation_events; DELETE FROM generation_tasks; DELETE FROM assets; DELETE FROM messages; DELETE FROM conversations;',
  );
});

function post(
  path: string,
  body: unknown,
  userId: number | null = ALICE,
): Promise<globalThis.Response> {
  return fetch(`${origin}${path}`, {
    method: 'POST',
    headers: {
      'content-type': 'application/json',
      ...(userId === null ? {} : { [AUTH_HEADER]: String(userId) }),
    },
    body: JSON.stringify(body ?? {}),
  });
}

function get(
  path: string,
  userId: number | null = ALICE,
  headers: Record<string, string> = {},
): Promise<globalThis.Response> {
  return fetch(`${origin}${path}`, {
    headers: { ...(userId === null ? {} : { [AUTH_HEADER]: String(userId) }), ...headers },
    redirect: 'manual',
  });
}

interface Frame {
  id: number | null;
  event: string;
  data: Record<string, unknown>;
}

/** Reads SSE frames off a stream until `count` arrived, then drops it. */
async function readFrames(response: globalThis.Response, count: number): Promise<Frame[]> {
  const body = response.body;
  if (body === null) {
    throw new Error('stream has no body');
  }
  const reader = body.getReader();
  const decoder = new TextDecoder();
  let buffer = '';
  const frames: Frame[] = [];

  while (frames.length < count) {
    const { done, value } = await reader.read();
    if (done) {
      break;
    }
    buffer += decoder.decode(value, { stream: true });
    let boundary = buffer.indexOf('\n\n');
    while (boundary !== -1) {
      const block = buffer.slice(0, boundary);
      buffer = buffer.slice(boundary + 2);
      let id: number | null = null;
      let event = '';
      let data: Record<string, unknown> = {};
      for (const line of block.split('\n')) {
        if (line.startsWith(':')) {
          continue;
        } else if (line.startsWith('id: ')) {
          id = Number(line.slice(4));
        } else if (line.startsWith('event: ')) {
          event = line.slice(7);
        } else if (line.startsWith('data: ')) {
          data = JSON.parse(line.slice(6)) as Record<string, unknown>;
        }
      }
      if (event.length > 0) {
        frames.push({ id, event, data });
      }
      boundary = buffer.indexOf('\n\n');
    }
  }
  // The stream never ends on its own; cancelling releases the server handler.
  await reader.cancel();
  return frames;
}

/** A conversation of `userId` holding one real file on disk. */
function ownedAsset(userId: number): {
  conversationId: number;
  assetId: number;
  filePath: string;
} {
  const conversationId = conversations.create(userId, 'q');
  const messageId = conversations.createAssistantPlaceholder(conversationId, 'image');
  const filePath = join(filesDir, `asset-${userId}.png`);
  writeFileSync(filePath, 'fake-png-bytes');
  const assetId = conversations.addAsset(messageId, {
    kind: 'image',
    filePath,
    sourceUrl: 'https://provider.example/expiring.png',
    mime: 'image/png',
    bytes: 14,
  });
  return { conversationId, assetId, filePath };
}

describe('POST /api/v1/conversations', () => {
  it('未登录返回 401', async () => {
    const response = await post('/api/v1/conversations', { prompt: 'hi' }, null);
    expect(response.status).toBe(401);
  });

  it('创建归属调用者的会话，标题取自 prompt 前缀', async () => {
    const response = await post('/api/v1/conversations', { prompt: '画一只红隼，要黄昏的光' });
    expect(response.status).toBe(201);
    const created = (await response.json()) as { conversationId: number };
    expect(conversations.isOwnedBy(created.conversationId, ALICE)).toBe(true);
    const list = conversations.listByUser(ALICE);
    expect(list[0]?.title.length).toBeLessThanOrEqual(30);
    expect(list[0]?.title.startsWith('画一只红隼')).toBe(true);
  });

  it('prompt 缺失或非法时用默认标题，仍是合法会话', async () => {
    for (const body of [{}, { prompt: 42 }, { prompt: '   '}]) {
      const response = await post('/api/v1/conversations', body);
      expect(response.status).toBe(201);
    }
    expect(conversations.listByUser(ALICE)).toHaveLength(3);
  });
});

describe('POST /api/v1/conversations/:id/turns — 提交即返回', () => {
  it('未登录返回 401，且不启动轮次', async () => {
    const mine = conversations.create(ALICE, 'q');
    const response = await post(`/api/v1/conversations/${mine}/turns`, { prompt: 'hi' }, null);
    expect(response.status).toBe(401);
    expect(runCalls).toEqual([]);
  });

  it('prompt 缺失或只有空白返回 400', async () => {
    const mine = conversations.create(ALICE, 'q');
    for (const prompt of [undefined, '', '   ', 42]) {
      const response = await post(`/api/v1/conversations/${mine}/turns`, { prompt });
      expect(response.status).toBe(400);
      await expect(response.json()).resolves.toEqual({ message: 'prompt 不能为空' });
    }
    expect(runCalls).toEqual([]);
  });

  it('202 返回会话与轮次的助手消息 id；轮次行已落库，回答稍后到', async () => {
    const mine = conversations.create(ALICE, 'q');
    const response = await post(`/api/v1/conversations/${mine}/turns`, {
      prompt: '  画一只红隼  ',
      mode: 'image',
    });
    expect(response.status).toBe(202);
    const body = (await response.json()) as { conversationId: number; messageId: number };
    expect(body.conversationId).toBe(mine);

    expect(runCalls).toHaveLength(1);
    expect(runCalls[0]?.conversationId).toBe(mine);
    expect(runCalls[0]?.mode).toBe('image');
    expect(runCalls[0]?.messageId).toBe(body.messageId);

    // The submit is not the answer's lifetime: the user row and the empty
    // placeholder exist the moment 202 goes out.
    const views = conversations.views(mine);
    expect(views.map((view) => view.role)).toEqual(['user', 'assistant']);
    expect(views[0]?.content).toBe('画一只红隼');
    expect(views[1]?.content).toBe('');
  });

  it('非法 id 的路由返回 404', async () => {
    expect((await post('/api/v1/conversations/abc/turns', { prompt: 'hi' })).status).toBe(404);
    expect((await post('/api/v1/conversations/0/turns', { prompt: 'hi' })).status).toBe(404);
  });

  it('写只认属主：别人的会话 404，且对方历史不动', async () => {
    // The old surface silently created a fresh conversation instead; with the
    // id in the path, inventing a different one could land a stale `?c=` turn
    // somewhere the user did not mean.
    const theirs = conversations.create(BOB, 'bob 的会话');
    const response = await post(`/api/v1/conversations/${theirs}/turns`, { prompt: '偷看' }, ALICE);
    expect(response.status).toBe(404);
    expect(conversations.views(theirs)).toEqual([]);
    expect(runCalls).toEqual([]);
  });

  it('超管写不进别人的会话，读得进自己的', async () => {
    const theirs = conversations.create(BOB, 'bob 的会话');
    expect(
      (await post(`/api/v1/conversations/${theirs}/turns`, { prompt: '插一句' }, ROOT)).status,
    ).toBe(404);

    const mine = conversations.create(ROOT, 'q');
    const response = await post(`/api/v1/conversations/${mine}/turns`, { prompt: '超管自己的' }, ROOT);
    expect(response.status).toBe(202);
  });

  it('会话已有轮次在跑时 409，不重复计费', async () => {
    const mine = conversations.create(ALICE, 'q');
    expect((await post(`/api/v1/conversations/${mine}/turns`, { prompt: '第一问' })).status).toBe(202);
    const second = await post(`/api/v1/conversations/${mine}/turns`, { prompt: '抢跑' });
    expect(second.status).toBe(409);
    await expect(second.json()).resolves.toEqual({ message: '该会话已有轮次在进行中' });
    expect(runCalls).toHaveLength(1);

    await finishRunningTurn();
    expect((await post(`/api/v1/conversations/${mine}/turns`, { prompt: '第二问' })).status).toBe(202);
  });

  it('不同会话各自跑，互不 409', async () => {
    const a = conversations.create(ALICE, 'a');
    const b = conversations.create(ALICE, 'b');
    expect((await post(`/api/v1/conversations/${a}/turns`, { prompt: '一' })).status).toBe(202);
    expect((await post(`/api/v1/conversations/${b}/turns`, { prompt: '二' })).status).toBe(202);
  });

  it('mode 解析与回退沿用轮次语义', async () => {
    const mine = conversations.create(ALICE, 'q');
    const cases: [unknown, Mode][] = [
      ['image', 'image'],
      ['videooo', 'auto'],
      ['  VIDEO ', 'video'],
      [undefined, 'auto'],
    ];
    for (const [mode, expected] of cases) {
      await post(`/api/v1/conversations/${mine}/turns`, { prompt: 'hi', mode });
      expect(runCalls.at(-1)?.mode).toBe(expected);
      await finishRunningTurn();
    }
  });

  it('日志顺序：user_message 先于 turn_started，且 seq 从 1 起', async () => {
    const mine = conversations.create(ALICE, 'q');
    const response = await post(`/api/v1/conversations/${mine}/turns`, { prompt: '画只隼' });
    const { messageId } = (await response.json()) as { messageId: number };

    const journal = events.replayAfter(mine, 0);
    expect(journal.map((entry) => entry.seq)).toEqual([1, 2]);
    expect(journal.map((entry) => entry.event.type)).toEqual(['user_message', 'turn_started']);
    const started = journal[1]?.event;
    expect(started?.type === 'turn_started' && started.messageId).toBe(messageId);
    const echoed = journal[0]?.event;
    expect(echoed?.type === 'user_message' && echoed.message.content).toBe('画只隼');
  });
});

describe('启动恢复：上个进程留下的占位行', () => {
  it('空 assistant 占位在 bootstrap 时被收尾，不再可能留下孤儿用户消息', async () => {
    const mine = conversations.create(ALICE, 'q');
    conversations.appendUserMessage(mine, '上一轮的问题', 'chat');
    const stale = conversations.createAssistantPlaceholder(mine, 'chat');

    await runner.onApplicationBootstrap();

    const view = conversations.messageView(stale);
    expect(view?.content.length).toBeGreaterThan(0);
  });

  it('已完成的正文不误伤', async () => {
    const mine = conversations.create(ALICE, 'q');
    const done = conversations.appendAssistantMessage(mine, '好的', 'chat');
    await runner.onApplicationBootstrap();
    expect(conversations.messageView(done)?.content).toBe('好的');
  });
});

describe('GET /api/v1/conversations/:id/stream', () => {
  it('首帧 connected 不带 id（它是这条连接的事，不是会话的事）', async () => {
    const mine = conversations.create(ALICE, 'q');
    const response = await get(`/api/v1/conversations/${mine}/stream`);
    expect(response.status).toBe(200);
    expect(response.headers.get('content-type')).toContain('text/event-stream');

    const frames = await readFrames(response, 1);
    expect(frames[0]).toEqual({ id: null, event: 'connected', data: { conversationId: mine } });
  });

  it('未登录 401，别人的 404，超管读得到普通账号的', async () => {
    const mine = conversations.create(ALICE, 'q');
    expect((await get(`/api/v1/conversations/${mine}/stream`, null)).status).toBe(401);

    const theirs = conversations.create(BOB, 'q');
    expect((await get(`/api/v1/conversations/${theirs}/stream`, ALICE)).status).toBe(404);

    const rootChat = conversations.create(ROOT, 'q');
    expect((await get(`/api/v1/conversations/${rootChat}/stream`, ALICE)).status).toBe(404);

    const asRoot = await get(`/api/v1/conversations/${mine}/stream`, ROOT);
    expect(asRoot.status).toBe(200);
    await readFrames(asRoot, 1);
  });

  it('活轮次：先开流后提交，事件带会话内 seq', async () => {
    const mine = conversations.create(ALICE, 'q');
    const response = await get(`/api/v1/conversations/${mine}/stream?from=0`);

    await post(`/api/v1/conversations/${mine}/turns`, { prompt: '画只隼' });
    const frames = await readFrames(response, 3);
    expect(frames.map((frame) => [frame.id, frame.event])).toEqual([
      [null, 'connected'],
      [1, 'user_message'],
      [2, 'turn_started'],
    ]);
  });

  it('?from= 定位首连：只重放游标之后的事件', async () => {
    const mine = conversations.create(ALICE, 'q');
    events.publish(mine, { type: 'turn_done', messageId: 10 });
    events.publish(mine, { type: 'message_added', message: viewOf(mine, 11) });

    const response = await get(`/api/v1/conversations/${mine}/stream?from=1`);
    const frames = await readFrames(response, 2);
    expect(frames.map((frame) => [frame.id, frame.event])).toEqual([
      [null, 'connected'],
      [2, 'message_added'],
    ]);
  });

  it('Last-Event-ID 优先于 ?from=（重连游标比页面游标新）', async () => {
    const mine = conversations.create(ALICE, 'q');
    events.publish(mine, { type: 'turn_done', messageId: 10 });
    events.publish(mine, { type: 'turn_done', messageId: 11 });
    events.publish(mine, { type: 'turn_done', messageId: 12 });

    const response = await get(`/api/v1/conversations/${mine}/stream?from=0`, ALICE, {
      'Last-Event-ID': '2',
    });
    const frames = await readFrames(response, 2);
    expect(frames.map((frame) => [frame.id, frame.event])).toEqual([
      [null, 'connected'],
      [3, 'turn_done'],
    ]);
    expect(frames[1]?.data).toEqual({ type: 'turn_done', messageId: 12 });
  });

  it('游标落在已裁剪窗口之下时发 resync：宁可让客户端重建，不给半个回放', async () => {
    const mine = conversations.create(ALICE, 'q');
    for (let i = 0; i < 4; i += 1) {
      events.publish(mine, { type: 'turn_done', messageId: i });
    }
    // Simulate the end-of-turn prune: the two oldest events are gone.
    connection.prepare('DELETE FROM conversation_events WHERE conversation_id = ? AND seq <= 2').run(mine);

    const response = await get(`/api/v1/conversations/${mine}/stream?from=0`);
    const frames = await readFrames(response, 4);
    expect(frames.map((frame) => [frame.id, frame.event])).toEqual([
      [null, 'resync'],
      [null, 'connected'],
      [3, 'turn_done'],
      [4, 'turn_done'],
    ]);
  });

  it('无游标连上跑着的会话：从未完轮次的第一件事之前追，SSR 画过的用户回声不再重放', async () => {
    const mine = conversations.create(ALICE, 'q');
    await post(`/api/v1/conversations/${mine}/turns`, { prompt: '正在画' });
    events.publish(mine, { type: 'text', messageId: runCalls[0]?.messageId ?? 0, delta: '一半了' });

    const response = await get(`/api/v1/conversations/${mine}/stream`);
    const frames = await readFrames(response, 3);
    // user_message(seq 1) was painted by SSR; the catch-up cursor sits right
    // before turn_started(seq 2).
    expect(frames.map((frame) => [frame.id, frame.event])).toEqual([
      [null, 'connected'],
      [2, 'turn_started'],
      [3, 'text'],
    ]);
  });

  it('data 载荷与实时一致：事件对象原样，含 type', async () => {
    const mine = conversations.create(ALICE, 'q');
    const event: ConversationEvent = {
      type: 'task_updated',
      taskId: 7,
      kind: 'video',
      status: 'running',
      error: null,
    };
    events.publish(mine, event);

    const response = await get(`/api/v1/conversations/${mine}/stream?from=0`);
    const frames = await readFrames(response, 2);
    expect(frames[1]?.data).toEqual(event);
  });
});

/** A minimal MessageView for journal-only assertions. */
function viewOf(conversationId: number, messageId: number) {
  void conversationId;
  return {
    id: messageId,
    role: 'assistant' as const,
    content: '后台完成',
    reasoning: null,
    toolCalls: null,
    mode: 'video' as const,
    createdAt: 0,
    assets: [],
  };
}

describe('GET /api/v1/conversations', () => {
  it('只列出自己的会话', async () => {
    const mine = conversations.create(ALICE, 'alice 的');
    conversations.create(BOB, 'bob 的');

    const list: { id: number; title: string }[] = await (await get('/api/v1/conversations')).json();
    expect(list.map((item) => item.id)).toEqual([mine]);
  });

  it('未登录返回 401', async () => {
    expect((await get('/api/v1/conversations', null)).status).toBe(401);
  });
});

describe('GET /api/v1/conversations/:id/messages', () => {
  it('返回自己的消息', async () => {
    const mine = conversations.create(ALICE, 'q');
    conversations.appendUserMessage(mine, '问', 'chat');

    const messages: { content: string; role: string }[] = await (
      await get(`/api/v1/conversations/${mine}/messages`)
    ).json();
    expect(messages).toHaveLength(1);
    expect(messages[0]).toMatchObject({ role: 'user', content: '问' });
  });

  it('别人的会话返回空数组而不是 403', async () => {
    // Deliberate: an empty list leaks less than a distinguishable error.
    const theirs = conversations.create(BOB, 'q');
    conversations.appendUserMessage(theirs, 'bob 的秘密', 'chat');

    await expect(
      (await get(`/api/v1/conversations/${theirs}/messages`)).json(),
    ).resolves.toEqual([]);
  });

  it('非法 id 返回空数组', async () => {
    await expect((await get('/api/v1/conversations/abc/messages')).json()).resolves.toEqual([]);
  });
});

describe('GET /api/v1/conversations/:id/tasks', () => {
  it('只返回本会话的活跃任务', async () => {
    const mine = conversations.create(ALICE, 'q');
    const theirs = conversations.create(BOB, 'q');
    const active = tasks.create({
      conversationId: mine,
      messageId: null,
      kind: 'video',
      providerTaskId: 'a',
      model: 'm',
      prompt: 'p',
      params: {},
    });
    tasks.create({
      conversationId: theirs,
      messageId: null,
      kind: 'video',
      providerTaskId: 'b',
      model: 'm',
      prompt: 'p',
      params: {},
    });

    const list: { id: number }[] = await (await get(`/api/v1/conversations/${mine}/tasks`)).json();
    expect(list.map((item) => item.id)).toEqual([active]);
  });

  it('别人的会话返回空数组', async () => {
    const theirs = conversations.create(BOB, 'q');
    tasks.create({
      conversationId: theirs,
      messageId: null,
      kind: 'video',
      providerTaskId: 'b',
      model: 'm',
      prompt: 'p',
      params: {},
    });

    await expect((await get(`/api/v1/conversations/${theirs}/tasks`)).json()).resolves.toEqual([]);
  });
});

describe('GET /api/v1/assets/:id/download', () => {
  it('属主可以取到文件本身', async () => {
    const { assetId } = ownedAsset(ALICE);

    const response = await get(`/api/v1/assets/${assetId}/download`);
    expect(response.status).toBe(200);
    expect(response.headers.get('content-type')).toContain('image/png');
    expect(response.headers.get('content-disposition')).toContain('inline');
    await expect(response.text()).resolves.toBe('fake-png-bytes');
  });

  it('非属主得到 404', async () => {
    const { assetId } = ownedAsset(BOB);
    expect((await get(`/api/v1/assets/${assetId}/download`, ALICE)).status).toBe(404);
  });

  it('不存在的资产与非法 id 得到 404', async () => {
    expect((await get('/api/v1/assets/9999/download')).status).toBe(404);
    expect((await get('/api/v1/assets/abc/download')).status).toBe(404);
  });
});

describe('POST /api/v1/conversations/:id/delete', () => {
  it('属主删除是软删除：自己的历史里消失，数据与文件都还在', async () => {
    const { conversationId, filePath } = ownedAsset(ALICE);
    conversations.appendUserMessage(conversationId, '问', 'chat');

    const response = await post(`/api/v1/conversations/${conversationId}/delete`, undefined, ALICE);
    expect(response.status).toBe(200);
    await expect(response.json()).resolves.toEqual({ ok: true });
    expect(conversations.listByUser(ALICE)).toEqual([]);
    expect(conversations.isOwnedBy(conversationId, ALICE)).toBe(false);
    expect(conversations.views(conversationId)).toHaveLength(2);
    expect(existsSync(filePath)).toBe(true);
  });

  it('超管删不掉 —— 删除只对普通账号开放', async () => {
    const { conversationId } = ownedAsset(ROOT);
    expect((await post(`/api/v1/conversations/${conversationId}/delete`, undefined, ROOT)).status).toBe(403);
    expect(conversations.isOwnedBy(conversationId, ROOT)).toBe(true);
  });

  it('软删除之后超管仍然读得到，属主读不到', async () => {
    const { conversationId } = ownedAsset(ALICE);
    conversations.appendUserMessage(conversationId, '被隐藏的问题', 'chat');
    await post(`/api/v1/conversations/${conversationId}/delete`, undefined, ALICE);

    const asRoot: { content: string }[] = await (
      await get(`/api/v1/conversations/${conversationId}/messages`, ROOT)
    ).json();
    // ownedAsset leaves an unfinished assistant placeholder, hence the contains.
    expect(asRoot.map((item) => item.content)).toContain('被隐藏的问题');

    await expect(
      (await get(`/api/v1/conversations/${conversationId}/messages`, ALICE)).json(),
    ).resolves.toEqual([]);
  });

  it('删别人的会话得到 404，且对方的会话还在', async () => {
    const { conversationId } = ownedAsset(BOB);
    expect((await post(`/api/v1/conversations/${conversationId}/delete`, undefined, ALICE)).status).toBe(404);
    expect(conversations.isOwnedBy(conversationId, BOB)).toBe(true);
  });

  it('超管删不掉别人的', async () => {
    const { conversationId } = ownedAsset(BOB);
    expect((await post(`/api/v1/conversations/${conversationId}/delete`, undefined, ROOT)).status).toBe(403);
    expect(conversations.isOwnedBy(conversationId, BOB)).toBe(true);
  });

  it('会话不存在或 id 非法时得到 404', async () => {
    expect((await post('/api/v1/conversations/9999/delete', undefined, ALICE)).status).toBe(404);
    expect((await post('/api/v1/conversations/0/delete', undefined, ALICE)).status).toBe(404);
  });

  it('未登录得到 401', async () => {
    const { conversationId } = ownedAsset(ALICE);
    expect((await post(`/api/v1/conversations/${conversationId}/delete`, undefined, null)).status).toBe(401);
  });
});

describe('超管的读取范围', () => {
  it('messages：读得到普通账号的会话，普通账号读不到超管的', async () => {
    const theirs = conversations.create(BOB, 'q');
    conversations.appendUserMessage(theirs, 'bob 的秘密', 'chat');

    const asRoot: { content: string }[] = await (
      await get(`/api/v1/conversations/${theirs}/messages`, ROOT)
    ).json();
    expect(asRoot.map((item) => item.content)).toEqual(['bob 的秘密']);

    const rootChat = conversations.create(ROOT, 'q');
    conversations.appendUserMessage(rootChat, '超管的会话', 'chat');
    for (const caller of [ALICE, BOB]) {
      await expect(
        (await get(`/api/v1/conversations/${rootChat}/messages`, caller)).json(),
      ).resolves.toEqual([]);
    }
  });

  it('assets：超管取得到普通账号的资产文件', async () => {
    const { assetId } = ownedAsset(BOB);
    const response = await get(`/api/v1/assets/${assetId}/download`, ROOT);
    expect(response.status).toBe(200);
    await expect(response.text()).resolves.toBe('fake-png-bytes');
  });

  it('会话列表不因超管而变成全站列表', async () => {
    const mine = conversations.create(ROOT, '我的');
    conversations.create(BOB, 'bob 的');

    const list: { id: number }[] = await (await get('/api/v1/conversations', ROOT)).json();
    expect(list.map((item) => item.id)).toEqual([mine]);
  });
});
