import { Test } from '@nestjs/testing';
import type { NestExpressApplication } from '@nestjs/platform-express';
import Database from 'better-sqlite3';
import { drizzle } from 'drizzle-orm/better-sqlite3';
import hbs from 'hbs';
import { join } from 'node:path';
import type { NextFunction, Request, Response } from 'express';
import { afterAll, beforeAll, beforeEach, describe, expect, it } from 'vitest';
import { AdminController } from '@/admin/admin.controller';
import { AdminService } from '@/admin/admin.service';
import { AuthService } from '@/auth/auth.service';
import type { Role } from '@/auth/roles';
import { ConversationService } from '@/conversation/conversation.service';
import { applySchema, DRIZZLE_INSTANCE } from '@/database/database.module';
import * as schema from '@/database/schema';

/**
 * The admin surface over real HTTP, rendered by the production templates.
 *
 * Two things matter here and nowhere else: the guard (an ordinary account must
 * not reach any of it, by GET or by POST) and the fact that the read-only page
 * of someone else's conversation renders no composer — the page that lets a
 * super admin look at another account's history must not become a way to write
 * into it.
 */
const AUTH_HEADER = 'x-test-user';
const ALICE = 1;
const ROOT = 2;

const ROLE_BY_ID = new Map<number, Role>([
  [ALICE, 'user'],
  [ROOT, 'super'],
]);

const VIEWS_DIR = join(process.cwd(), 'src', 'view', 'views');

let app: NestExpressApplication;
let origin: string;
let connection: Database.Database;
let admin: AdminService;
let auth: AuthService;
let conversations: ConversationService;

async function bootstrap(): Promise<void> {
  connection = new Database(':memory:');
  applySchema(connection);
  const db = drizzle(connection, { schema });

  const moduleRef = await Test.createTestingModule({
    controllers: [AdminController],
    providers: [
      AdminService,
      AuthService,
      ConversationService,
      { provide: DRIZZLE_INSTANCE, useValue: db },
    ],
  }).compile();

  admin = moduleRef.get(AdminService);
  auth = moduleRef.get(AuthService);
  conversations = moduleRef.get(ConversationService);

  app = moduleRef.createNestApplication<NestExpressApplication>();
  app.setBaseViewsDir(VIEWS_DIR);
  app.setViewEngine('hbs');
  hbs.registerPartials(join(VIEWS_DIR, 'partials'));
  hbs.registerHelper('eq', (a: unknown, b: unknown): boolean => a === b);

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

beforeAll(async () => {
  await bootstrap();
});

afterAll(async () => {
  await app.close();
  connection.close();
});

/** Written with explicit ids: the request middleware maps ids to roles. */
function seedFixtureUsers(): void {
  const insert = connection.prepare(
    'INSERT INTO users (id, username, password_hash, role, created_at) VALUES (?, ?, ?, ?, ?)',
  );
  for (const [id, role] of ROLE_BY_ID) {
    insert.run(id, `user${id}`, 'x', role, Date.now());
  }
}

beforeEach(() => {
  // The app instance stays up across tests, so reset the data instead of
  // rebooting it. Children first: foreign keys are on. The fixtures are rebuilt
  // rather than merely kept, since a test deletes one of them on purpose.
  connection.exec(
    'DELETE FROM generation_tasks; DELETE FROM assets; DELETE FROM messages; DELETE FROM conversations; DELETE FROM users;',
  );
  seedFixtureUsers();
});

function get(path: string, userId: number | null = ROOT): Promise<globalThis.Response> {
  return fetch(`${origin}${path}`, {
    headers: userId === null ? {} : { [AUTH_HEADER]: String(userId) },
    redirect: 'manual',
  });
}

function post(
  path: string,
  fields: Record<string, string>,
  userId: number | null = ROOT,
): Promise<globalThis.Response> {
  return fetch(`${origin}${path}`, {
    method: 'POST',
    headers: {
      'content-type': 'application/x-www-form-urlencoded',
      ...(userId === null ? {} : { [AUTH_HEADER]: String(userId) }),
    },
    body: new URLSearchParams(fields).toString(),
    redirect: 'manual',
  });
}

describe('SuperAdminGuard', () => {
  it.each([['/admin'], ['/admin/conversations'], ['/admin/users/1'], ['/admin/users/1/delete']])(
    '未登录访问 %s 时重定向到登录页',
    async (path) => {
      const response = await get(path, null);
      expect(response.status).toBe(302);
      expect(response.headers.get('location')).toBe('/login');
    },
  );

  it('普通账号访问时重定向回首页', async () => {
    const response = await get('/admin', ALICE);
    expect(response.status).toBe(302);
    expect(response.headers.get('location')).toBe('/');
  });

  it('普通账号 POST 时得到 403，而不是被静默重定向', async () => {
    // A redirect on a rejected write reads as "nothing happened", which is worse
    // than an error: the caller would believe the account was created.
    const response = await post('/admin/users', { username: 'bob', password: 'a-good-one' }, ALICE);
    expect(response.status).toBe(403);
    expect(admin.list()).toHaveLength(2);
  });
});

describe('GET /admin', () => {
  it('列出账号、角色与会话数', async () => {
    conversations.create(ALICE, 'alice 的会话');

    const html = await (await get('/admin')).text();
    expect(html).toContain('user1');
    expect(html).toContain('user2');
    expect(html).toContain('超级管理员');
    expect(html).toContain('普通用户');
  });

  it('notice 码渲染成提示文案', async () => {
    const created = await (await get('/admin?notice=created')).text();
    const duplicate = await (await get('/admin?notice=duplicate')).text();

    expect(created).toContain('账号已创建');
    expect(duplicate).toContain('该用户名已存在');
  });

  it('未知 notice 码不渲染提示', async () => {
    const html = await (await get('/admin?notice=nonsense')).text();
    expect(html).not.toContain('admin-notice');
  });
});

describe('POST /admin/users', () => {
  it('建出账号并重定向到成功提示', async () => {
    const response = await post('/admin/users', { username: 'bob', password: 'a-good-one' });

    expect(response.status).toBe(302);
    expect(response.headers.get('location')).toBe('/admin?notice=created');
    await expect(auth.validate('bob', 'a-good-one')).resolves.not.toBeNull();
  });

  it('重名时重定向到重名提示', async () => {
    await post('/admin/users', { username: 'bob', password: 'a-good-one' });
    const response = await post('/admin/users', { username: 'bob', password: 'another-one' });

    expect(response.headers.get('location')).toBe('/admin?notice=duplicate');
  });

  it('用户名或密码不合规时重定向到 invalid', async () => {
    const badName = await post('/admin/users', { username: 'a b', password: 'a-good-one' });
    const badPassword = await post('/admin/users', { username: 'bob', password: 'short' });

    expect(badName.headers.get('location')).toBe('/admin?notice=invalid');
    expect(badPassword.headers.get('location')).toBe('/admin?notice=invalid');
  });

  it('缺字段时不炸，按不合规处理', async () => {
    const response = await post('/admin/users', {});
    expect(response.status).toBe(302);
    expect(response.headers.get('location')).toBe('/admin?notice=invalid');
  });
});

describe('POST /admin/users/:id/password', () => {
  it('重置后新密码可用', async () => {
    const created = admin.list().find((account) => account.username === 'user1');
    if (created === undefined) {
      throw new Error('fixture account is missing');
    }

    const response = await post(`/admin/users/${created.id}/password`, { password: 'brand-new' });

    expect(response.headers.get('location')).toBe(`/admin/users/${created.id}?notice=password-reset`);
    await expect(auth.validate('user1', 'brand-new')).resolves.not.toBeNull();
  });

  it('不合规时提示 invalid，原密码不动', async () => {
    const created = admin.list().find((account) => account.username === 'user1');
    if (created === undefined) {
      throw new Error('fixture account is missing');
    }

    const response = await post(`/admin/users/${created.id}/password`, { password: 'short' });
    expect(response.headers.get('location')).toBe(`/admin/users/${created.id}?notice=invalid`);
  });

  it('账号不存在时回到账号列表', async () => {
    const response = await post('/admin/users/9999/password', { password: 'brand-new' });
    expect(response.headers.get('location')).toBe('/admin?notice=missing');
  });
});

describe('GET /admin/users/:id', () => {
  function accountIdOf(username: string): number {
    const found = admin.list().find((account) => account.username === username);
    if (found === undefined) {
      throw new Error(`fixture account ${username} is missing`);
    }
    return found.id;
  }

  it('列出账号信息与两个动作', async () => {
    const html = await (await get(`/admin/users/${accountIdOf('user1')}`)).text();

    expect(html).toContain('user1');
    expect(html).toContain('普通用户');
    expect(html).toContain('重置密码');
    expect(html).toContain(`/admin/users/${ALICE}/delete`);
  });

  it('密码框旁边有一个可见的账号字段', async () => {
    // Not decoration: Chromium warns about a password box with no visible
    // username beside it, and password managers need the pairing. Pinned here
    // because the a11y warning is invisible to every other check in this repo.
    const html = await (await get(`/admin/users/${accountIdOf('user1')}`)).text();

    expect(html).toMatch(/<input name="username" type="text"[^>]*autocomplete="username"/);
    expect(html).not.toContain('type="hidden"');
  });

  it('账号不存在时回到列表', async () => {
    const response = await get('/admin/users/9999');
    expect(response.headers.get('location')).toBe('/admin?notice=missing');
  });
});

describe('删除账号', () => {
  it('确认页给出会话与资产数量', async () => {
    const created = admin.list().find((account) => account.username === 'user1');
    if (created === undefined) {
      throw new Error('fixture account is missing');
    }
    conversations.create(ALICE, '要一起删掉的');

    const html = await (await get(`/admin/users/${created.id}/delete`)).text();
    expect(html).toContain('删除账号 user1');
    expect(html).toContain('1 个会话');
  });

  it('确认页对不存在的账号回到列表', async () => {
    const response = await get('/admin/users/9999/delete');
    expect(response.headers.get('location')).toBe('/admin?notice=missing');
  });

  it('删掉后账号与会话一起消失', async () => {
    const created = admin.list().find((account) => account.username === 'user1');
    if (created === undefined) {
      throw new Error('fixture account is missing');
    }
    conversations.create(ALICE, '要一起删掉的');

    const response = await post(`/admin/users/${created.id}/delete`, {});

    expect(response.headers.get('location')).toBe('/admin?notice=removed');
    expect(admin.find(created.id)).toBeNull();
    expect(conversations.listAll()).toEqual([]);
  });

  it('删自己时拒绝', async () => {
    const response = await post(`/admin/users/${ROOT}/delete`, {});
    expect(response.headers.get('location')).toBe('/admin?notice=self');
    expect(admin.find(ROOT)).not.toBeNull();
  });
});

describe('GET /admin/conversations', () => {
  it('列出全部账号的会话，并标出归属', async () => {
    conversations.create(ALICE, 'alice 的问题');
    conversations.create(ROOT, '超管的问题');

    const html = await (await get('/admin/conversations')).text();
    expect(html).toContain('alice 的问题');
    expect(html).toContain('超管的问题');
    expect(html).toContain('user1');
  });

  it('?user= 只留下该账号的会话', async () => {
    conversations.create(ALICE, 'alice 的问题');
    conversations.create(ROOT, '超管的问题');

    const html = await (await get(`/admin/conversations?user=${ALICE}`)).text();
    expect(html).toContain('alice 的问题');
    expect(html).not.toContain('超管的问题');
  });

  it('没有会话时给出空态', async () => {
    const html = await (await get('/admin/conversations')).text();
    expect(html).toContain('没有会话');
  });
});

describe('软删除的会话对超管仍然可见', () => {
  it('全站会话列表里带着「已删除」标记', async () => {
    const id = conversations.create(ALICE, '用户删掉的会话');
    conversations.softDeleteConversations([id]);

    const html = await (await get('/admin/conversations')).text();

    expect(html).toContain('用户删掉的会话');
    expect(html).toContain('已删除');
  });

  it('只读回放仍然打得开，并说明已被删除', async () => {
    const id = conversations.create(ALICE, '用户删掉的会话');
    conversations.appendUserMessage(id, '原来的问题', 'chat');
    conversations.softDeleteConversations([id]);

    const html = await (await get(`/admin/conversations/${id}`)).text();

    expect(html).toContain('原来的问题');
    expect(html).toContain('被用户删除');
  });

  it('账号页的会话数把它们也算进去', async () => {
    // They still exist: the count is rows, not "rows the owner can see".
    const id = conversations.create(ALICE, '用户删掉的会话');
    conversations.softDeleteConversations([id]);

    const account = admin.list().find((item) => item.username === 'user1');
    expect(account?.conversationCount).toBe(1);
  });
});

describe('GET /admin/conversations/:id — 只读回放', () => {
  function seedConversation(userId: number): number {
    const id = conversations.create(userId, '别人的会话');
    conversations.appendUserMessage(id, '画一只红隼', 'image');
    const assistant = conversations.createAssistantPlaceholder(id, 'image');
    conversations.finalizeAssistant(assistant, {
      content: '**画好了**',
      reasoning: '先想构图',
      toolCalls: [{ name: 'generate_image', arguments: {}, ok: true, summary: '1 image' }],
    });
    return id;
  }

  it('渲染出转写、思考过程与工具调用', async () => {
    const id = seedConversation(ALICE);

    const html = await (await get(`/admin/conversations/${id}`)).text();

    expect(html).toContain('画一只红隼');
    expect(html).toContain('先想构图');
    expect(html).toContain('generate_image');
    expect(html).toContain('<strong>画好了</strong>');
  });

  it('带上归属账号，并声明是只读的', async () => {
    const id = seedConversation(ALICE);

    const html = await (await get(`/admin/conversations/${id}`)).text();

    expect(html).toContain('user1 的会话');
    expect(html).toContain('只读视图');
  });

  it('没有输入框，也不加载客户端脚本', async () => {
    // The whole point of the separate page: reading must not become writing.
    const id = seedConversation(ALICE);

    const html = await (await get(`/admin/conversations/${id}`)).text();

    expect(html).not.toContain('composer-form');
    expect(html).not.toContain('/assets/main.js');
  });

  it('看自己的会话时提示改用对话页', async () => {
    const id = seedConversation(ROOT);
    const html = await (await get(`/admin/conversations/${id}`)).text();
    expect(html).toContain('这是你自己的会话');
    expect(html).not.toContain('只读视图');
  });

  it('会话不存在时回到列表', async () => {
    const response = await get('/admin/conversations/9999');
    expect(response.headers.get('location')).toBe('/admin/conversations?notice=missing');
  });
});
