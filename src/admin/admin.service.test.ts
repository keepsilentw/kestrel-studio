import { Test } from '@nestjs/testing';
import Database from 'better-sqlite3';
import { drizzle } from 'drizzle-orm/better-sqlite3';
import { existsSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it } from 'vitest';
import { AdminService } from '@/admin/admin.service';
import { AuthService } from '@/auth/auth.service';
import { ConversationService } from '@/conversation/conversation.service';
import { applySchema, DRIZZLE_INSTANCE } from '@/database/database.module';
import * as schema from '@/database/schema';

/**
 * Account management against a real in-memory SQLite: what is under test is the
 * cascade, the refusal rules and the password hashing, none of which a stubbed
 * drizzle would reproduce.
 *
 * STORAGE_DIR is redirected to a temp directory so the cascade's file deletion
 * is exercised without writing into the project's own storage/.
 */
interface Harness {
  admin: AdminService;
  auth: AuthService;
  conversations: ConversationService;
  seedUser(username: string, role?: 'user' | 'super'): number;
  raw: Database.Database;
}

let harness: Harness;
let filesDir: string;
let previousStorageDir: string | undefined;

async function buildHarness(): Promise<Harness> {
  const connection = new Database(':memory:');
  applySchema(connection);
  const db = drizzle(connection, { schema });

  const moduleRef = await Test.createTestingModule({
    providers: [
      AdminService,
      AuthService,
      ConversationService,
      { provide: DRIZZLE_INSTANCE, useValue: db },
    ],
  }).compile();

  return {
    admin: moduleRef.get(AdminService),
    auth: moduleRef.get(AuthService),
    conversations: moduleRef.get(ConversationService),
    raw: connection,
    seedUser(username: string, role: 'user' | 'super' = 'user'): number {
      const result = db
        .insert(schema.users)
        .values({ username, passwordHash: 'not-a-real-hash', role, createdAt: new Date() })
        .run();
      return Number(result.lastInsertRowid);
    },
  };
}

beforeAll(() => {
  filesDir = mkdtempSync(join(tmpdir(), 'kestrel-admin-'));
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

/** Creates an account, failing the test if the fixture itself was rejected. */
async function createAccount(username: string, password: string): Promise<number> {
  const result = await harness.admin.create(username, password);
  if (!result.ok) {
    throw new Error(`fixture account ${username} was refused: ${result.reason}`);
  }
  return result.id;
}

/** A conversation of `userId` holding one asset whose bytes exist on disk. */
function seedAsset(userId: number, fileName: string, filePath?: string): number {
  const conversationId = harness.conversations.create(userId, '会话');
  const messageId = harness.conversations.createAssistantPlaceholder(conversationId, 'image');
  const path = filePath ?? join(filesDir, fileName);
  writeFileSync(path, 'bytes');
  return harness.conversations.addAsset(messageId, {
    kind: 'image',
    filePath: path,
    sourceUrl: 'https://provider.example/a.png',
    mime: 'image/png',
    bytes: 5,
  });
}

describe('AdminService.create', () => {
  it('建出普通账号，密码可登录', async () => {
    const result = await harness.admin.create('alice', 'a-good-password');
    expect(result).toEqual({ ok: true, id: expect.any(Number) });

    const principal = await harness.auth.validate('alice', 'a-good-password');
    expect(principal?.role).toBe('user');
  });

  it('用户名两端空白去掉', async () => {
    await harness.admin.create('  alice  ', 'a-good-password');
    await expect(harness.auth.validate('alice', 'a-good-password')).resolves.not.toBeNull();
  });

  it('重名时拒绝', async () => {
    await harness.admin.create('alice', 'a-good-password');
    await expect(harness.admin.create('alice', 'another-password')).resolves.toEqual({
      ok: false,
      reason: 'duplicate',
    });
  });

  it.each([['ab'], ['a b'], ['名字'], ['a'.repeat(33)], ['']])(
    '用户名 %s 不合规时拒绝',
    async (username) => {
      await expect(harness.admin.create(username, 'a-good-password')).resolves.toEqual({
        ok: false,
        reason: 'username',
      });
    },
  );

  it.each([['short'], ['      '], ['x'.repeat(129)], ['']])(
    '密码不合规时拒绝',
    async (password) => {
      await expect(harness.admin.create('alice', password)).resolves.toEqual({
        ok: false,
        reason: 'password',
      });
    },
  );

  it('即使名字看起来像管理员，也建成普通账号', async () => {
    // Privilege is a column, never a naming convention.
    await harness.admin.create('superadmin', 'a-good-password');
    const principal = await harness.auth.validate('superadmin', 'a-good-password');
    expect(principal?.role).toBe('user');
  });
});

describe('AdminService.resetPassword', () => {
  it('改完旧密码失效、新密码可用', async () => {
    const id = await createAccount('alice', 'old-password');

    await expect(harness.admin.resetPassword(id, 'new-password')).resolves.toEqual({ ok: true });
    await expect(harness.auth.validate('alice', 'old-password')).resolves.toBeNull();
    await expect(harness.auth.validate('alice', 'new-password')).resolves.not.toBeNull();
  });

  it('不合规的密码被拒，且原密码仍然可用', async () => {
    const id = await createAccount('alice', 'old-password');

    await expect(harness.admin.resetPassword(id, 'short')).resolves.toEqual({
      ok: false,
      reason: 'password',
    });
    await expect(harness.auth.validate('alice', 'old-password')).resolves.not.toBeNull();
  });

  it('账号不存在时返回 missing', async () => {
    await expect(harness.admin.resetPassword(9999, 'a-good-password')).resolves.toEqual({
      ok: false,
      reason: 'missing',
    });
  });
});

describe('AdminService.remove', () => {
  it('删掉账号及其会话、消息、资产与文件', async () => {
    const alice = harness.seedUser('alice');
    const assetId = seedAsset(alice, 'owned.png');
    const assetPath = join(filesDir, 'owned.png');
    const conversationId = harness.conversations.create(alice, '会话');
    harness.conversations.appendUserMessage(conversationId, '问', 'chat');
    expect(existsSync(assetPath)).toBe(true);

    const root = harness.seedUser('root', 'super');
    await expect(harness.admin.remove(alice, { id: root, role: 'super' })).resolves.toEqual({
      ok: true,
      conversations: 2,
      assets: 1,
    });

    expect(harness.admin.find(alice)).toBeNull();
    expect(harness.conversations.views(conversationId)).toEqual([]);
    expect(existsSync(assetPath)).toBe(false);
    const assets = harness.raw.prepare('SELECT COUNT(*) AS n FROM assets WHERE id = ?').get(assetId) as { n: number };
    expect(assets.n).toBe(0);
  });

  it('不动 STORAGE_DIR 之外的文件', async () => {
    // Only rows written before the column held a bare name can point outside the
    // storage directory; a delete must not become an arbitrary unlink for them.
    const alice = harness.seedUser('alice');
    const outside = join(tmpdir(), `kestrel-outside-${process.pid}.png`);
    writeFileSync(outside, 'bytes');
    const assetId = seedAsset(alice, 'irrelevant.png');
    harness.raw
      .prepare('UPDATE assets SET file_path = ? WHERE id = ?')
      .run(outside, assetId);

    const root = harness.seedUser('root', 'super');
    await expect(harness.admin.remove(alice, { id: root, role: 'super' })).resolves.toEqual({
      ok: true,
      conversations: 1,
      assets: 1,
    });

    expect(existsSync(outside)).toBe(true);
    rmSync(outside, { force: true });
  });

  it('搬迁前写下的绝对路径仍能删掉文件', async () => {
    const alice = harness.seedUser('alice');
    const legacy = join(filesDir, 'legacy.png');
    writeFileSync(legacy, 'bytes');
    const assetId = seedAsset(alice, 'ignored.png');
    harness.raw.prepare('UPDATE assets SET file_path = ? WHERE id = ?').run(legacy, assetId);

    const root = harness.seedUser('root', 'super');
    await harness.admin.remove(alice, { id: root, role: 'super' });

    expect(existsSync(legacy)).toBe(false);
  });

  it('删不掉自己', async () => {
    const root = harness.seedUser('root', 'super');
    await expect(harness.admin.remove(root, { id: root, role: 'super' })).resolves.toEqual({
      ok: false,
      reason: 'self',
    });
    expect(harness.admin.find(root)).not.toBeNull();
  });

  it('删不掉最后一个超管', async () => {
    const root = harness.seedUser('root', 'super');
    const root2 = harness.seedUser('root2', 'super');
    // root2 acting on root is allowed while there are two...
    await expect(harness.admin.remove(root, { id: root2, role: 'super' })).resolves.toMatchObject({
      ok: true,
    });
    // ...and refused once root2 is the only one. Deleting itself is caught first,
    // so act as a normal account to reach the last-super branch.
    const alice = harness.seedUser('alice');
    await expect(harness.admin.remove(root2, { id: alice, role: 'user' })).resolves.toEqual({
      ok: false,
      reason: 'last-super',
    });
    expect(harness.admin.find(root2)).not.toBeNull();
  });

  it('账号不存在时返回 missing', async () => {
    const root = harness.seedUser('root', 'super');
    await expect(harness.admin.remove(9999, { id: root, role: 'super' })).resolves.toEqual({
      ok: false,
      reason: 'missing',
    });
  });

  it('没有会话的账号也能删掉', async () => {
    const alice = harness.seedUser('alice');
    const root = harness.seedUser('root', 'super');
    await expect(harness.admin.remove(alice, { id: root, role: 'super' })).resolves.toEqual({
      ok: true,
      conversations: 0,
      assets: 0,
    });
  });
});

describe('AdminService.list / find', () => {
  it('列出全部账号，带上会话与资产数', async () => {
    const alice = harness.seedUser('alice');
    const root = harness.seedUser('root', 'super');
    harness.conversations.create(alice, '一');
    harness.conversations.create(alice, '二');
    seedAsset(alice, 'a.png');

    const accounts = harness.admin.list();
    expect(accounts).toHaveLength(2);
    expect(accounts[0]).toMatchObject({
      id: alice,
      username: 'alice',
      role: 'user',
      roleLabel: '普通用户',
      conversationCount: 3,
      assetCount: 1,
    });
    expect(accounts[1]).toMatchObject({ id: root, role: 'super', conversationCount: 0 });
  });

  it('find 返回单个账号，找不到时为 null', () => {
    const alice = harness.seedUser('alice');
    expect(harness.admin.find(alice)?.username).toBe('alice');
    expect(harness.admin.find(9999)).toBeNull();
  });
});

describe('AdminService 的存储目录判定', () => {
  it('测试确实把 STORAGE_DIR 指到了临时目录', () => {
    // Guards the harness itself: if this ever fails, the cascade test above is
    // unlinking inside the project's real storage/.
    expect(resolve(process.env.STORAGE_DIR ?? '')).toBe(filesDir);
  });
});
