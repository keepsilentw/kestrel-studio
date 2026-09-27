import { Test } from '@nestjs/testing';
import Database from 'better-sqlite3';
import { drizzle } from 'drizzle-orm/better-sqlite3';
import { existsSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Role, Viewer } from '@/auth/roles';
import type { ToolCallRecord } from '@/conversation/conversation.service';
import { ConversationService } from '@/conversation/conversation.service';
import { applySchema, DRIZZLE_INSTANCE } from '@/database/database.module';
import * as schema from '@/database/schema';

/**
 * Runs against a real in-memory SQLite with the production DDL applied — the
 * schema, the join scoping and the JSON columns are exactly what is under test,
 * so stubbing drizzle would test nothing.
 *
 * The connection is opened here rather than through `openDatabase()`: that path
 * runs DATABASE_FILE through `resolve()`, so `:memory:` becomes a real file
 * named ":memory:".
 *
 * STORAGE_DIR points at a temp directory: asset rows store a bare file name and
 * are resolved against it, so the tests need a directory they own.
 */
let filesDir: string;
let previousStorageDir: string | undefined;

interface Harness {
  service: ConversationService;
  seedUser(username: string, role?: Role): number;
  raw: Database.Database;
}

let harness: Harness;

/** The requester half of the visibility rule; the owner role comes from the row. */
function viewer(id: number, role: Role = 'user'): Viewer {
  return { id, role };
}

async function buildHarness(): Promise<Harness> {
  const connection = new Database(':memory:');
  applySchema(connection);
  const db = drizzle(connection, { schema });

  const moduleRef = await Test.createTestingModule({
    providers: [ConversationService, { provide: DRIZZLE_INSTANCE, useValue: db }],
  }).compile();

  return {
    service: moduleRef.get(ConversationService),
    raw: connection,
    seedUser(username: string, role: Role = 'user'): number {
      const result = db
        .insert(schema.users)
        .values({ username, passwordHash: 'not-a-real-hash', role, createdAt: new Date() })
        .run();
      return Number(result.lastInsertRowid);
    },
  };
}

beforeAll(() => {
  filesDir = mkdtempSync(join(tmpdir(), 'kestrel-conv-'));
  previousStorageDir = process.env.STORAGE_DIR;
  process.env.STORAGE_DIR = filesDir;
});

afterAll(() => {
  if (previousStorageDir === undefined) {
    delete process.env.STORAGE_DIR;
  } else {
    process.env.STORAGE_DIR = previousStorageDir;
  }
  rmSync(filesDir, { recursive: true, force: true });
});

beforeEach(async () => {
  harness = await buildHarness();
});

afterEach(() => {
  harness.raw.close();
});

describe('ConversationService.create', () => {
  it('用首句作为标题', () => {
    const userId = harness.seedUser('u1');
    const id = harness.service.create(userId, '画一只站在岩石上的红隼');
    expect(harness.service.listByUser(userId)[0]).toMatchObject({
      id,
      title: '画一只站在岩石上的红隼',
    });
  });

  it('标题截断到 30 字', () => {
    const userId = harness.seedUser('u1');
    harness.service.create(userId, '一'.repeat(50));
    expect(harness.service.listByUser(userId)[0].title).toHaveLength(30);
  });

  it('空白首句回退为默认标题', () => {
    const userId = harness.seedUser('u1');
    harness.service.create(userId, '   ');
    expect(harness.service.listByUser(userId)[0].title).toBe('新会话');
  });

  it('标题两侧空白被去掉', () => {
    const userId = harness.seedUser('u1');
    harness.service.create(userId, '  红隼  ');
    expect(harness.service.listByUser(userId)[0].title).toBe('红隼');
  });
});

describe('ConversationService 归属', () => {
  it('isOwnedBy 只认自己的会话', () => {
    const alice = harness.seedUser('alice');
    const bob = harness.seedUser('bob');
    const id = harness.service.create(alice, 'hi');

    expect(harness.service.isOwnedBy(id, alice)).toBe(true);
    // The authorization primitive every conversation endpoint calls by hand.
    expect(harness.service.isOwnedBy(id, bob)).toBe(false);
    expect(harness.service.isOwnedBy(9999, alice)).toBe(false);
  });

  it('ownerOf 返回属主，不存在时返回 null', () => {
    const alice = harness.seedUser('alice');
    const id = harness.service.create(alice, 'hi');
    expect(harness.service.ownerOf(id)).toBe(alice);
    expect(harness.service.ownerOf(9999)).toBeNull();
  });

  it('listByUser 不串号', () => {
    const alice = harness.seedUser('alice');
    const bob = harness.seedUser('bob');
    harness.service.create(alice, 'alice 的会话');
    harness.service.create(bob, 'bob 的会话');

    expect(harness.service.listByUser(alice).map((c) => c.title)).toEqual(['alice 的会话']);
  });

  it('touch 把会话顶到最前', () => {
    // Fake timers, not a backdated row: touch() writes `new Date()`, so on a
    // real clock the bumped conversation can land in the same millisecond it
    // was created and the assertion would be decided by the id tie-break
    // instead of by time.
    vi.useFakeTimers();
    try {
      vi.setSystemTime(new Date('2026-09-22T10:00:00Z'));
      const alice = harness.seedUser('alice');
      const first = harness.service.create(alice, '第一个');

      vi.setSystemTime(new Date('2026-09-22T10:00:05Z'));
      const second = harness.service.create(alice, '第二个');
      expect(harness.service.listByUser(alice).map((c) => c.id)).toEqual([second, first]);

      vi.setSystemTime(new Date('2026-09-22T10:00:10Z'));
      harness.service.touch(first);
      expect(harness.service.listByUser(alice).map((c) => c.id)).toEqual([first, second]);
    } finally {
      vi.useRealTimers();
    }
  });

  it('updatedAt 相同时按 id 倒序，顺序确定', () => {
    const alice = harness.seedUser('alice');
    const first = harness.service.create(alice, '第一个');
    const second = harness.service.create(alice, '第二个');
    // Pin both to the same instant so the tie is guaranteed rather than
    // incidental. Without the id tie-break this order comes out of the scan
    // instead, and changes when the query plan does.
    harness.raw
      .prepare('UPDATE conversations SET updated_at = ? WHERE id IN (?, ?)')
      .run(Date.now(), first, second);

    expect(harness.service.listByUser(alice).map((c) => c.id)).toEqual([second, first]);
  });
});

describe('ConversationService.buildInputItems', () => {
  it('按 id 顺序回放用户与助手消息', () => {
    const userId = harness.seedUser('u1');
    const conversationId = harness.service.create(userId, 'q');
    harness.service.appendUserMessage(conversationId, '第一问', 'chat');
    const assistant = harness.service.createAssistantPlaceholder(conversationId, 'chat');
    harness.service.finalizeAssistant(assistant, {
      content: '第一答',
      reasoning: null,
      toolCalls: null,
    });

    expect(harness.service.buildInputItems(conversationId)).toEqual([
      { role: 'user', content: '第一问' },
      { role: 'assistant', content: '第一答' },
    ]);
  });

  it('跳过内容为空的占位行', () => {
    // This is half of the defence against the incident recorded in
    // agent.service.ts: an unfinalized assistant placeholder left in the table
    // would otherwise replay as a blank assistant turn, and a second user
    // message would follow it back-to-back.
    const userId = harness.seedUser('u1');
    const conversationId = harness.service.create(userId, 'q');
    harness.service.appendUserMessage(conversationId, '问', 'chat');
    harness.service.createAssistantPlaceholder(conversationId, 'chat');

    expect(harness.service.buildInputItems(conversationId)).toEqual([{ role: 'user', content: '问' }]);
  });

  it('跳过只有空白的消息', () => {
    const userId = harness.seedUser('u1');
    const conversationId = harness.service.create(userId, 'q');
    harness.service.appendUserMessage(conversationId, '   ', 'chat');
    expect(harness.service.buildInputItems(conversationId)).toEqual([]);
  });

  it('不跨会话混入消息', () => {
    const userId = harness.seedUser('u1');
    const a = harness.service.create(userId, 'a');
    const b = harness.service.create(userId, 'b');
    harness.service.appendUserMessage(a, '属于 a', 'chat');
    harness.service.appendUserMessage(b, '属于 b', 'chat');

    expect(harness.service.buildInputItems(a)).toEqual([{ role: 'user', content: '属于 a' }]);
  });

  it('不回放工具往返', () => {
    // Documented in the method: the call ids would need the provider-side item
    // log, and the outcome is already in the assistant text.
    const userId = harness.seedUser('u1');
    const conversationId = harness.service.create(userId, 'q');
    const assistant = harness.service.createAssistantPlaceholder(conversationId, 'image');
    harness.service.finalizeAssistant(assistant, {
      content: '画好了',
      reasoning: null,
      toolCalls: [{ name: 'generate_image', arguments: {}, ok: true, summary: 'done' }],
    });

    expect(harness.service.buildInputItems(conversationId)).toEqual([
      { role: 'assistant', content: '画好了' },
    ]);
  });
});

describe('ConversationService.finalizeAssistant', () => {
  it('写入正文、思考过程与工具调用', () => {
    const userId = harness.seedUser('u1');
    const conversationId = harness.service.create(userId, 'q');
    const id = harness.service.createAssistantPlaceholder(conversationId, 'image');
    const toolCalls: ToolCallRecord[] = [
      { name: 'generate_image', arguments: { prompt: 'x' }, ok: true, summary: '1 image' },
    ];
    harness.service.finalizeAssistant(id, { content: '完成', reasoning: '想一想', toolCalls });

    const view = harness.service.messageView(id);
    expect(view?.content).toBe('完成');
    expect(view?.reasoning).toBe('想一想');
    expect(view?.toolCalls).toEqual(toolCalls);
    expect(view?.mode).toBe('image');
  });

  it('toolCalls 为 null 时不写入 JSON 字面量 "null"', () => {
    const userId = harness.seedUser('u1');
    const conversationId = harness.service.create(userId, 'q');
    const id = harness.service.createAssistantPlaceholder(conversationId, 'chat');
    harness.service.finalizeAssistant(id, { content: '答', reasoning: null, toolCalls: null });

    expect(harness.service.messageView(id)?.toolCalls).toBeNull();
  });
});

describe('ConversationService 消息回放容错', () => {
  it('tool_calls 列里是坏 JSON 时退化为 null 而不是抛错', () => {
    const userId = harness.seedUser('u1');
    const conversationId = harness.service.create(userId, 'q');
    const id = harness.service.appendUserMessage(conversationId, '问', 'chat');
    harness.raw.prepare('UPDATE messages SET tool_calls = ? WHERE id = ?').run('{not json', id);

    expect(() => harness.service.views(conversationId)).not.toThrow();
    expect(harness.service.views(conversationId)[0].toolCalls).toBeNull();
  });

  it('tool_calls 是 JSON 但不是数组时同样退化为 null', () => {
    const userId = harness.seedUser('u1');
    const conversationId = harness.service.create(userId, 'q');
    const id = harness.service.appendUserMessage(conversationId, '问', 'chat');
    harness.raw.prepare('UPDATE messages SET tool_calls = ? WHERE id = ?').run('{"a":1}', id);

    expect(harness.service.views(conversationId)[0].toolCalls).toBeNull();
  });
});

describe('ConversationService 资产', () => {
  const storedImage = {
    kind: 'image' as const,
    filePath: '/app/storage/image-1.png',
    sourceUrl: 'https://provider.example/expiring.png',
    mime: 'image/png',
    bytes: 2048,
  };

  function withImage(): {
    userId: number;
    conversationId: number;
    messageId: number;
    assetId: number;
  } {
    const userId = harness.seedUser('u1');
    const conversationId = harness.service.create(userId, 'q');
    const messageId = harness.service.createAssistantPlaceholder(conversationId, 'image');
    const assetId = harness.service.addAsset(messageId, storedImage);
    return { userId, conversationId, messageId, assetId };
  }

  it('回放时资产挂在对应消息上，url 指向下载端点', () => {
    const { conversationId, assetId } = withImage();
    const view = harness.service.views(conversationId).find((m) => m.assets.length > 0);

    expect(view?.assets).toEqual([
      { id: assetId, kind: 'image', url: `/api/assets/${assetId}/download`, mime: 'image/png', bytes: 2048 },
    ]);
  });

  it('回放不外泄磁盘路径与供应商 URL', () => {
    // Provider URLs expire, so they are never handed to the browser; the file
    // path must not leak either.
    const { conversationId } = withImage();
    const serialized = JSON.stringify(harness.service.views(conversationId));
    expect(serialized).not.toContain('/app/storage');
    expect(serialized).not.toContain('provider.example');
  });

  it('imageAssetIn 限定在本会话内', () => {
    const { conversationId, assetId } = withImage();
    expect(harness.service.imageAssetIn(conversationId, assetId)?.id).toBe(assetId);
    expect(harness.service.imageAssetIn(conversationId + 999, assetId)).toBeNull();
  });

  it('imageAssetIn 拒绝非图片资产', () => {
    const userId = harness.seedUser('u1');
    const conversationId = harness.service.create(userId, 'q');
    const messageId = harness.service.createAssistantPlaceholder(conversationId, 'video');
    const videoId = harness.service.addAsset(messageId, {
      ...storedImage,
      kind: 'video',
      filePath: '/app/storage/video-1.mp4',
      mime: 'video/mp4',
    });

    // Image-to-video takes a first frame, so anything but an image must miss.
    expect(harness.service.imageAssetIn(conversationId, videoId)).toBeNull();
  });

  it('findAssetForViewer 只让属主取到', () => {
    const { assetId } = withImage();
    const other = harness.seedUser('other');
    expect(harness.service.findAssetForViewer(assetId, viewer(other))).toBeNull();
    expect(harness.service.findAssetForViewer(9999, viewer(other))).toBeNull();
  });

  it('findAssetForViewer 给属主返回磁盘路径供 sendFile 使用', () => {
    const { userId, assetId } = withImage();
    expect(harness.service.findAssetForViewer(assetId, viewer(userId))?.filePath).toBe(
      join(filesDir, 'image-1.png'),
    );
  });

  it('入库存的是文件名，不是写盘时的绝对路径', () => {
    // The row has to survive the project moving, so the directory must not be
    // part of it.
    const { assetId } = withImage();
    const row = harness.raw.prepare('SELECT file_path AS p FROM assets WHERE id = ?').get(assetId);
    expect(row).toEqual({ p: 'image-1.png' });
  });

  it('搬迁前写下的绝对路径：原处没有该文件时按当前 STORAGE_DIR 解析', () => {
    // The regression this covers: after the project left playground/, every
    // asset row pointed into the old directory and 404'd for the owner too.
    const { userId, assetId } = withImage();
    const moved = join(filesDir, 'legacy.png');
    writeFileSync(moved, 'bytes');
    harness.raw
      .prepare('UPDATE assets SET file_path = ? WHERE id = ?')
      .run(join(tmpdir(), 'kestrel-gone', 'legacy.png'), assetId);

    expect(harness.service.findAssetForViewer(assetId, viewer(userId))?.filePath).toBe(moved);
    rmSync(moved, { force: true });
  });

  it('搬迁前的绝对路径：原处还在时原样沿用', () => {
    const { userId, assetId } = withImage();
    const stillThere = join(filesDir, 'unmoved.png');
    writeFileSync(stillThere, 'bytes');
    harness.raw.prepare('UPDATE assets SET file_path = ? WHERE id = ?').run(stillThere, assetId);

    expect(harness.service.findAssetForViewer(assetId, viewer(userId))?.filePath).toBe(stillThere);
    rmSync(stillThere, { force: true });
  });

  it('findAssetForViewer 跟着会话的可见性走：超管能取普通账号的资产', () => {
    const { assetId } = withImage();
    const root = harness.seedUser('root', 'super');
    expect(harness.service.findAssetForViewer(assetId, viewer(root, 'super'))).not.toBeNull();
  });

  it('assetFile 刻意不做归属校验 —— 授权来自签名串', () => {
    const { assetId } = withImage();
    expect(harness.service.assetFile(assetId)).toEqual({
      filePath: join(filesDir, 'image-1.png'),
      mime: 'image/png',
    });
    expect(harness.service.assetFile(9999)).toBeNull();
  });
});

/**
 * The visibility rule in full. Its shape is "own, or — for a super admin — any
 * normal account's", so every pair of roles needs pinning: the interesting
 * failure is not "someone sees nothing" but "someone sees the wrong thing".
 */
describe('ConversationService.canRead', () => {
  it('普通账号只读得到自己的', () => {
    const alice = harness.seedUser('alice');
    const bob = harness.seedUser('bob');
    const aliceChat = harness.service.create(alice, 'a');
    const bobChat = harness.service.create(bob, 'b');

    expect(harness.service.canRead(aliceChat, viewer(alice))).toBe(true);
    expect(harness.service.canRead(bobChat, viewer(alice))).toBe(false);
  });

  it('超管读得到普通账号的', () => {
    const alice = harness.seedUser('alice');
    const root = harness.seedUser('root', 'super');
    const aliceChat = harness.service.create(alice, 'a');

    expect(harness.service.canRead(aliceChat, viewer(root, 'super'))).toBe(true);
  });

  it('超管读不到另一个超管的', () => {
    const root = harness.seedUser('root', 'super');
    const otherRoot = harness.seedUser('other-root', 'super');
    const theirs = harness.service.create(otherRoot, 'x');

    expect(harness.service.canRead(theirs, viewer(root, 'super'))).toBe(false);
  });

  it('超管自己的会话仍然读得到', () => {
    const root = harness.seedUser('root', 'super');
    const mine = harness.service.create(root, 'mine');

    expect(harness.service.canRead(mine, viewer(root, 'super'))).toBe(true);
  });

  it('普通账号读不到超管的', () => {
    const root = harness.seedUser('root', 'super');
    const alice = harness.seedUser('alice');
    const rootChat = harness.service.create(root, 'x');

    expect(harness.service.canRead(rootChat, viewer(alice))).toBe(false);
  });

  it('会话不存在时为 false', () => {
    const alice = harness.seedUser('alice');
    expect(harness.service.canRead(9999, viewer(alice))).toBe(false);
    expect(harness.service.canRead(9999, viewer(alice, 'super'))).toBe(false);
  });

  it('写路径仍然只看属主 —— 超管也写不进别人的会话', () => {
    const alice = harness.seedUser('alice');
    const root = harness.seedUser('root', 'super');
    const aliceChat = harness.service.create(alice, 'a');

    expect(harness.service.isOwnedBy(aliceChat, root)).toBe(false);
  });
});

/**
 * The one cascade behind both "delete my conversation" and "delete an account".
 * Its contract is that nothing is left behind — a message or an asset row whose
 * parent went away is exactly the kind of residue nobody notices until a foreign
 * key complains or storage fills up.
 */
describe('ConversationService.deleteConversations', () => {
  function seedConversationWithAsset(
    userId: number,
    fileName: string,
  ): { conversationId: number; filePath: string } {
    const conversationId = harness.service.create(userId, 'q');
    const messageId = harness.service.createAssistantPlaceholder(conversationId, 'image');
    const filePath = join(filesDir, fileName);
    writeFileSync(filePath, 'bytes');
    harness.service.addAsset(messageId, {
      kind: 'image',
      filePath,
      sourceUrl: 'https://provider.example/a.png',
      mime: 'image/png',
      bytes: 5,
    });
    harness.service.appendUserMessage(conversationId, '问', 'chat');
    return { conversationId, filePath };
  }

  function seedTask(conversationId: number): void {
    harness.raw
      .prepare(
        `INSERT INTO generation_tasks
           (conversation_id, kind, provider_task_id, model, prompt, status, attempts, created_at, updated_at)
         VALUES (?, 'video', 'p1', 'm', 'prompt', 'running', 0, ?, ?)`,
      )
      .run(conversationId, Date.now(), Date.now());
  }

  it('删掉会话、消息、资产、任务与磁盘文件', async () => {
    const userId = harness.seedUser('u1');
    const { conversationId, filePath } = seedConversationWithAsset(userId, 'gone.png');
    seedTask(conversationId);

    await expect(harness.service.deleteConversations([conversationId])).resolves.toEqual({
      conversations: 1,
      assets: 1,
    });

    expect(harness.service.listByUser(userId)).toEqual([]);
    expect(harness.service.views(conversationId)).toEqual([]);
    expect(existsSync(filePath)).toBe(false);
    expect(
      harness.raw
        .prepare(
          `SELECT (SELECT COUNT(*) FROM messages) AS m,
                  (SELECT COUNT(*) FROM assets) AS a,
                  (SELECT COUNT(*) FROM generation_tasks) AS t`,
        )
        .get(),
    ).toEqual({ m: 0, a: 0, t: 0 });
  });

  it('只动列出来的会话，其它会话与文件原样', async () => {
    const userId = harness.seedUser('u1');
    const kept = seedConversationWithAsset(userId, 'kept.png');
    const doomed = harness.service.create(userId, '要删的');

    await harness.service.deleteConversations([doomed]);

    expect(harness.service.listByUser(userId).map((item) => item.id)).toEqual([
      kept.conversationId,
    ]);
    expect(existsSync(kept.filePath)).toBe(true);
  });

  it('空列表什么都不做', async () => {
    // The admin path reaches this when an account has no conversations at all.
    const userId = harness.seedUser('u1');
    const { conversationId } = seedConversationWithAsset(userId, 'still.png');

    await expect(harness.service.deleteConversations([])).resolves.toEqual({
      conversations: 0,
      assets: 0,
    });
    expect(harness.service.listByUser(userId).map((item) => item.id)).toEqual([conversationId]);
  });
});

/**
 * Soft delete: what the history dropdown does. The contract is asymmetric on
 * purpose — the owner loses sight of the conversation, the super admin does not —
 * and nothing is allowed to disappear from storage.
 */
describe('ConversationService.softDeleteConversations', () => {
  it('对属主隐藏，但数据一条不动', () => {
    const alice = harness.seedUser('alice');
    const conversationId = harness.service.create(alice, '画一只红隼');
    harness.service.appendUserMessage(conversationId, '画一只红隼', 'image');
    const messageId = harness.service.createAssistantPlaceholder(conversationId, 'image');
    harness.service.finalizeAssistant(messageId, { content: '画好了', reasoning: null, toolCalls: null });

    expect(harness.service.softDeleteConversations([conversationId])).toBe(1);

    expect(harness.service.listByUser(alice)).toEqual([]);
    expect(harness.service.isOwnedBy(conversationId, alice)).toBe(false);
    expect(harness.service.canRead(conversationId, viewer(alice))).toBe(false);
    // Still there, for the super admin and for the record.
    expect(harness.service.views(conversationId)).toHaveLength(2);
    expect(harness.service.canRead(conversationId, viewer(harness.seedUser('root', 'super'), 'super'))).toBe(true);
  });

  it('超管的列表里仍然在，且带上删除时间', () => {
    const alice = harness.seedUser('alice');
    const conversationId = harness.service.create(alice, '被隐藏的');
    harness.service.softDeleteConversations([conversationId]);

    const listed = harness.service.listAll().find((item) => item.id === conversationId);
    expect(listed?.deletedAt).toEqual(expect.any(Number));
    expect(harness.service.summaryOf(conversationId)?.deletedAt).toEqual(expect.any(Number));
  });

  it('重复删除只算一次', () => {
    const alice = harness.seedUser('alice');
    const conversationId = harness.service.create(alice, '一次就够');

    expect(harness.service.softDeleteConversations([conversationId])).toBe(1);
    expect(harness.service.softDeleteConversations([conversationId])).toBe(0);
  });

  it('空列表什么都不做', () => {
    expect(harness.service.softDeleteConversations([])).toBe(0);
  });

  it('只隐藏列出来的那个', () => {
    const alice = harness.seedUser('alice');
    const kept = harness.service.create(alice, '留着');
    const hidden = harness.service.create(alice, '隐藏');

    harness.service.softDeleteConversations([hidden]);

    expect(harness.service.listByUser(alice).map((item) => item.id)).toEqual([kept]);
  });
});

describe('ConversationService 的全站视图', () => {
  it('listAll 带上归属账号，按更新时间倒序', () => {
    const alice = harness.seedUser('alice');
    const bob = harness.seedUser('bob');
    const older = harness.service.create(alice, '早的');
    const newer = harness.service.create(bob, '晚的');
    harness.raw
      .prepare('UPDATE conversations SET updated_at = ? WHERE id = ?')
      .run(Date.now() - 60_000, older);

    expect(harness.service.listAll()).toEqual([
      {
        id: newer,
        title: '晚的',
        updatedAt: expect.any(Number),
        deletedAt: null,
        ownerId: bob,
        ownerName: 'bob',
      },
      {
        id: older,
        title: '早的',
        updatedAt: expect.any(Number),
        deletedAt: null,
        ownerId: alice,
        ownerName: 'alice',
      },
    ]);
  });

  it('summaryOf 取到归属，取不到时返回 null', () => {
    const alice = harness.seedUser('alice');
    const id = harness.service.create(alice, '会话');

    expect(harness.service.summaryOf(id)).toMatchObject({ id, ownerName: 'alice' });
    expect(harness.service.summaryOf(9999)).toBeNull();
  });
});
