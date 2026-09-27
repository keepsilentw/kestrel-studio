import { Test } from '@nestjs/testing';
import type { NestExpressApplication } from '@nestjs/platform-express';
import Database from 'better-sqlite3';
import { drizzle } from 'drizzle-orm/better-sqlite3';
import { existsSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import type { NextFunction, Request, Response } from 'express';
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import { AgentService } from '@/agent/agent.service';
import type { Mode } from '@/agent/mode';
import type { Role } from '@/auth/roles';
import { ChatController } from '@/chat/chat.controller';
import { ConversationService } from '@/conversation/conversation.service';
import { applySchema, DRIZZLE_INSTANCE } from '@/database/database.module';
import * as schema from '@/database/schema';
import { TaskEventsService } from '@/task/task-events.service';
import { TaskService } from '@/task/task.service';

/**
 * Exercised over real HTTP. The value here is the authorization layer: every
 * endpoint resolves the caller by hand (`canRead` on the reads, `isOwnedBy` on
 * the write), with no interceptor behind it, so a missing check is invisible
 * until it is probed from the outside.
 *
 * AgentService is the injected boundary and is stubbed — a real turn would call
 * the provider, and the agent loop is deliberately out of unit-test scope (see
 * vitest.config.mts). The stub closes the SSE writer, which is the one thing the
 * controller relies on AgentService to do.
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
  prompt: string;
  mode: Mode;
}

let app: NestExpressApplication;
let origin: string;
let connection: Database.Database;
let conversations: ConversationService;
let tasks: TaskService;
let runCalls: RunCall[];
let filesDir: string;

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
      TaskService,
      TaskEventsService,
      { provide: DRIZZLE_INSTANCE, useValue: db },
      {
        provide: AgentService,
        useValue: {
          run: async (options: RunCall & { writer: { close(): void } }): Promise<void> => {
            runCalls.push({
              conversationId: options.conversationId,
              prompt: options.prompt,
              mode: options.mode,
            });
            options.writer.close();
          },
        },
      },
    ],
  }).compile();

  conversations = moduleRef.get(ConversationService);
  tasks = moduleRef.get(TaskService);

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

beforeEach(() => {
  runCalls = [];
  // The app instance stays up across tests, so clear the history instead of
  // rebooting it. Children first: foreign keys are on.
  connection.exec(
    'DELETE FROM generation_tasks; DELETE FROM assets; DELETE FROM messages; DELETE FROM conversations;',
  );
});

function postChat(body: unknown, userId: number = ALICE): Promise<globalThis.Response> {
  return fetch(`${origin}/api/chat`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', [AUTH_HEADER]: String(userId) },
    body: JSON.stringify(body),
  });
}

function get(path: string, userId: number | null = ALICE): Promise<globalThis.Response> {
  return fetch(`${origin}${path}`, {
    headers: userId === null ? {} : { [AUTH_HEADER]: String(userId) },
    redirect: 'manual',
  });
}

async function framesOf(response: globalThis.Response): Promise<{ event: string; data: unknown }[]> {
  const text = await response.text();
  return text
    .split('\n\n')
    .map((block) => block.trim())
    .filter((block) => block.length > 0)
    .map((block) => {
      const lines = block.split('\n');
      const event = lines.find((line) => line.startsWith('event:'))?.slice('event:'.length).trim();
      const data = lines.find((line) => line.startsWith('data:'))?.slice('data:'.length).trim();
      return { event: event ?? '', data: JSON.parse(data ?? 'null') };
    });
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

describe('POST /api/chat — 入参校验', () => {
  it('没有身份时返回 401', async () => {
    const response = await fetch(`${origin}/api/chat`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ prompt: 'hi' }),
    });
    expect(response.status).toBe(401);
    expect(runCalls).toEqual([]);
  });

  it('prompt 缺失或只有空白返回 400，且不启动轮次', async () => {
    for (const prompt of [undefined, '', '   ']) {
      const response = await postChat({ prompt });
      expect(response.status).toBe(400);
      await expect(response.json()).resolves.toEqual({ message: 'prompt 不能为空' });
    }
    expect(runCalls).toEqual([]);
  });

  it('prompt 非字符串返回 400', async () => {
    const response = await postChat({ prompt: 42 });
    expect(response.status).toBe(400);
  });

  it('prompt 两端空白被去掉', async () => {
    await postChat({ prompt: '  画一只红隼  ' });
    expect(runCalls[0].prompt).toBe('画一只红隼');
  });
});

describe('POST /api/chat — 会话归属', () => {
  it('没有指定会话时新建，并把新 id 通过 connected 帧告知前端', async () => {
    const response = await postChat({ prompt: '画一只红隼' });
    expect(response.status).toBe(200);
    expect(response.headers.get('content-type')).toContain('text/event-stream');

    const frames = await framesOf(response);
    const connected = frames.find((frame) => frame.event === 'connected');
    expect(connected?.data).toEqual({ conversationId: runCalls[0].conversationId });
    expect(conversations.isOwnedBy(runCalls[0].conversationId, ALICE)).toBe(true);
  });

  it('指定自己的会话时复用', async () => {
    const mine = conversations.create(ALICE, '早先的会话');
    await postChat({ prompt: '继续', conversationId: mine });
    expect(runCalls[0].conversationId).toBe(mine);
  });

  it('指定别人的会话时新建，不写入他人会话', async () => {
    // The only thing standing between a forged conversationId and another
    // user's history.
    const theirs = conversations.create(BOB, 'bob 的会话');
    await postChat({ prompt: '偷看', conversationId: theirs }, ALICE);

    expect(runCalls[0].conversationId).not.toBe(theirs);
    expect(conversations.isOwnedBy(runCalls[0].conversationId, ALICE)).toBe(true);
    expect(conversations.views(theirs)).toEqual([]);
  });

  it.each([['abc'], ['-1'], ['0'], ['1.5'], ['null']])(
    '非法 conversationId %s 时新建',
    async (conversationId) => {
      await postChat({ prompt: 'hi', conversationId });
      expect(conversations.isOwnedBy(runCalls[0].conversationId, ALICE)).toBe(true);
    },
  );
});

describe('POST /api/chat — 模式', () => {
  it('把解析后的模式传给轮次', async () => {
    await postChat({ prompt: '画一只红隼', mode: 'image' });
    expect(runCalls[0].mode).toBe('image');
  });

  it('非法模式回退为 auto', async () => {
    await postChat({ prompt: 'hi', mode: 'videooo' });
    expect(runCalls[0].mode).toBe('auto');
  });

  it('大小写与空白不敏感', async () => {
    await postChat({ prompt: 'hi', mode: '  VIDEO ' });
    expect(runCalls[0].mode).toBe('video');
  });

  it('缺省模式为 auto', async () => {
    await postChat({ prompt: 'hi' });
    expect(runCalls[0].mode).toBe('auto');
  });
});

describe('GET /api/conversations', () => {
  it('只列出自己的会话', async () => {
    const mine = conversations.create(ALICE, 'alice 的');
    conversations.create(BOB, 'bob 的');

    const response = await get('/api/conversations');
    const list: { id: number; title: string }[] = await response.json();
    expect(list.map((item) => item.id)).toEqual([mine]);
  });

  it('未登录返回 401', async () => {
    expect((await get('/api/conversations', null)).status).toBe(401);
  });
});

describe('GET /api/conversations/:id/messages', () => {
  it('返回自己的消息', async () => {
    const mine = conversations.create(ALICE, 'q');
    conversations.appendUserMessage(mine, '问', 'chat');

    const response = await get(`/api/conversations/${mine}/messages`);
    const messages: { content: string; role: string }[] = await response.json();
    expect(messages).toHaveLength(1);
    expect(messages[0]).toMatchObject({ role: 'user', content: '问' });
  });

  it('别人的会话返回空数组而不是 403', async () => {
    // Deliberate: an empty list leaks less than a distinguishable error.
    const theirs = conversations.create(BOB, 'q');
    conversations.appendUserMessage(theirs, 'bob 的秘密', 'chat');

    const response = await get(`/api/conversations/${theirs}/messages`);
    await expect(response.json()).resolves.toEqual([]);
  });

  it('非法 id 返回空数组', async () => {
    await expect((await get('/api/conversations/abc/messages')).json()).resolves.toEqual([]);
  });
});

describe('GET /api/conversations/:id/tasks', () => {
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

    const response = await get(`/api/conversations/${mine}/tasks`);
    const list: { id: number }[] = await response.json();
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

    await expect((await get(`/api/conversations/${theirs}/tasks`)).json()).resolves.toEqual([]);
  });
});

describe('GET /api/assets/:id/download', () => {
  it('属主可以取到文件本身', async () => {
    const { assetId } = ownedAsset(ALICE);

    const response = await get(`/api/assets/${assetId}/download`);
    expect(response.status).toBe(200);
    expect(response.headers.get('content-type')).toContain('image/png');
    expect(response.headers.get('content-disposition')).toContain('inline');
    await expect(response.text()).resolves.toBe('fake-png-bytes');
  });

  it('非属主得到 404', async () => {
    const { assetId } = ownedAsset(BOB);

    const response = await get(`/api/assets/${assetId}/download`, ALICE);
    expect(response.status).toBe(404);
  });

  it('不存在的资产得到 404', async () => {
    expect((await get('/api/assets/9999/download')).status).toBe(404);
  });

  it('非法 id 得到 404', async () => {
    expect((await get('/api/assets/abc/download')).status).toBe(404);
  });
});

describe('GET /api/conversations/:id/events', () => {
  it('属主拿到事件流，首帧是 connected', async () => {
    const mine = conversations.create(ALICE, 'q');

    const response = await get(`/api/conversations/${mine}/events`);
    expect(response.status).toBe(200);
    expect(response.headers.get('content-type')).toContain('text/event-stream');

    const body = response.body;
    if (body === null) {
      throw new Error('event stream has no body');
    }
    const reader = body.getReader();
    const { value } = await reader.read();
    const first = new TextDecoder().decode(value);
    expect(first).toContain('event: connected');
    expect(first).toContain(`"conversationId":${mine}`);

    // The stream never ends on its own; cancelling releases the server handler.
    await reader.cancel();
  });

  it('非属主得到 404', async () => {
    const theirs = conversations.create(BOB, 'q');
    const response = await get(`/api/conversations/${theirs}/events`, ALICE);
    expect(response.status).toBe(404);
  });

  it('未登录得到 401', async () => {
    const mine = conversations.create(ALICE, 'q');
    expect((await get(`/api/conversations/${mine}/events`, null)).status).toBe(401);
  });
});

describe('POST /api/conversations/:id/delete', () => {
  function deleteConversation(
    id: number,
    userId: number | null = ALICE,
  ): Promise<globalThis.Response> {
    return fetch(`${origin}/api/conversations/${id}/delete`, {
      method: 'POST',
      headers: userId === null ? {} : { [AUTH_HEADER]: String(userId) },
      redirect: 'manual',
    });
  }

  it('属主删除是软删除：自己的历史里消失，数据与文件都还在', async () => {
    const { conversationId, filePath } = ownedAsset(ALICE);
    conversations.appendUserMessage(conversationId, '问', 'chat');

    const response = await deleteConversation(conversationId, ALICE);

    expect(response.status).toBe(200);
    await expect(response.json()).resolves.toEqual({ ok: true });
    // Gone from the owner's view…
    expect(conversations.listByUser(ALICE)).toEqual([]);
    expect(conversations.isOwnedBy(conversationId, ALICE)).toBe(false);
    // …but nothing was removed: the turn is still stored and the file is still there.
    expect(conversations.views(conversationId)).toHaveLength(2);
    expect(existsSync(filePath)).toBe(true);
  });

  it('超管删不掉 —— 删除只对普通账号开放', async () => {
    const { conversationId } = ownedAsset(ROOT);

    const response = await deleteConversation(conversationId, ROOT);

    expect(response.status).toBe(403);
    expect(conversations.isOwnedBy(conversationId, ROOT)).toBe(true);
  });

  it('软删除之后超管仍然读得到，属主读不到', async () => {
    // The asymmetry the soft delete exists for: the history stays auditable.
    const { conversationId } = ownedAsset(ALICE);
    conversations.appendUserMessage(conversationId, '被隐藏的问题', 'chat');
    await deleteConversation(conversationId, ALICE);

    const asRoot: { content: string }[] = await (
      await get(`/api/conversations/${conversationId}/messages`, ROOT)
    ).json();
    // ownedAsset leaves an unfinished assistant placeholder, hence the contains.
    expect(asRoot.map((item) => item.content)).toContain('被隐藏的问题');

    await expect(
      (await get(`/api/conversations/${conversationId}/messages`, ALICE)).json(),
    ).resolves.toEqual([]);
  });

  it('删别人的会话得到 404，且对方的会话还在', async () => {
    const { conversationId } = ownedAsset(BOB);

    const response = await deleteConversation(conversationId, ALICE);

    expect(response.status).toBe(404);
    expect(conversations.isOwnedBy(conversationId, BOB)).toBe(true);
  });

  it('超管删不掉别人的', async () => {
    const { conversationId } = ownedAsset(BOB);

    const response = await deleteConversation(conversationId, ROOT);

    expect(response.status).toBe(403);
    expect(conversations.isOwnedBy(conversationId, BOB)).toBe(true);
  });

  it('会话不存在或 id 非法时得到 404', async () => {
    expect((await deleteConversation(9999, ALICE)).status).toBe(404);
    expect((await deleteConversation(0, ALICE)).status).toBe(404);
  });

  it('未登录得到 401', async () => {
    const { conversationId } = ownedAsset(ALICE);
    expect((await deleteConversation(conversationId, null)).status).toBe(401);
  });
});

/**
 * The super admin's extra reach, and its limit. Reads widen; the write path does
 * not — the guard against posting into someone else's history has to hold for a
 * privileged account too, or a stray conversationId would append to it.
 */
describe('超管的读取范围', () => {
  it('messages：读得到普通账号的会话', async () => {
    const theirs = conversations.create(BOB, 'q');
    conversations.appendUserMessage(theirs, 'bob 的秘密', 'chat');

    const response = await get(`/api/conversations/${theirs}/messages`, ROOT);
    const messages: { content: string }[] = await response.json();
    expect(messages.map((item) => item.content)).toEqual(['bob 的秘密']);
  });

  it('messages：普通账号读不到超管的', async () => {
    const rootChat = conversations.create(ROOT, 'q');
    conversations.appendUserMessage(rootChat, '超管的会话', 'chat');

    for (const caller of [ALICE, BOB]) {
      await expect(
        (await get(`/api/conversations/${rootChat}/messages`, caller)).json(),
      ).resolves.toEqual([]);
    }
  });

  it('assets：取得到普通账号的资产', async () => {
    const { assetId } = ownedAsset(BOB);

    const response = await get(`/api/assets/${assetId}/download`, ROOT);
    expect(response.status).toBe(200);
    await expect(response.text()).resolves.toBe('fake-png-bytes');
  });

  it('tasks：取得到普通账号会话的活跃任务', async () => {
    const theirs = conversations.create(BOB, 'q');
    const taskId = tasks.create({
      conversationId: theirs,
      messageId: null,
      kind: 'video',
      providerTaskId: 'a',
      model: 'm',
      prompt: 'p',
      params: {},
    });

    const list: { id: number }[] = await (
      await get(`/api/conversations/${theirs}/tasks`, ROOT)
    ).json();
    expect(list.map((item) => item.id)).toEqual([taskId]);
  });

  it('events：订阅得到普通账号的会话', async () => {
    const theirs = conversations.create(BOB, 'q');

    const response = await get(`/api/conversations/${theirs}/events`, ROOT);
    expect(response.status).toBe(200);
    const body = response.body;
    if (body === null) {
      throw new Error('event stream has no body');
    }
    const reader = body.getReader();
    await reader.cancel();
  });

  it('写入仍然只认属主：超管指定别人的会话时新建', async () => {
    const theirs = conversations.create(BOB, 'bob 的会话');

    await postChat({ prompt: '插一句', conversationId: theirs }, ROOT);

    expect(runCalls[0].conversationId).not.toBe(theirs);
    expect(conversations.views(theirs)).toEqual([]);
  });

  it('会话列表不因超管而变成全站列表', async () => {
    const mine = conversations.create(ROOT, '我的');
    conversations.create(BOB, 'bob 的');

    const list: { id: number }[] = await (await get('/api/conversations', ROOT)).json();
    expect(list.map((item) => item.id)).toEqual([mine]);
  });
});
