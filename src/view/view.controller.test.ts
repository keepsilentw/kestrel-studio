import { Test } from '@nestjs/testing';
import type { NestExpressApplication } from '@nestjs/platform-express';
import Database from 'better-sqlite3';
import { drizzle } from 'drizzle-orm/better-sqlite3';
import hbs from 'hbs';
import { join } from 'node:path';
import type { NextFunction, Request, Response } from 'express';
import { afterAll, beforeAll, beforeEach, describe, expect, it } from 'vitest';
import { ConversationService } from '@/conversation/conversation.service';
import { applySchema, DRIZZLE_INSTANCE } from '@/database/database.module';
import * as schema from '@/database/schema';
import { TaskEventsService } from '@/task/task-events.service';
import { TaskService } from '@/task/task.service';
import { ViewController } from '@/view/view.controller';
import type { Role } from '@/auth/roles';

/**
 * Asserts against the actually rendered Handlebars output, with the view engine
 * wired exactly as src/main.ts wires it. That makes this the one place a
 * template change is caught: the client script cannot import server files, so
 * its own fixture (web/scripts/main.test.ts) can only mirror the contract.
 *
 * The scaffold test at the bottom is the counterpart — it pins the ids
 * web/scripts/main.ts looks up with requireElement().
 */
const AUTH_HEADER = 'x-test-user';
const ALICE = 1;
const BOB = 2;
const ROOT = 3;

/** The id→role mapping the request middleware hands out, mirrored in the rows. */
const ROLE_BY_ID = new Map<number, Role>([
  [ALICE, 'user'],
  [BOB, 'user'],
  [ROOT, 'super'],
]);

const VIEWS_DIR = join(process.cwd(), 'src', 'view', 'views');

let app: NestExpressApplication;
let origin: string;
let connection: Database.Database;
let conversations: ConversationService;
let tasks: TaskService;

async function bootstrap(): Promise<void> {
  connection = new Database(':memory:');
  applySchema(connection);
  const db = drizzle(connection, { schema });

  for (const [id, role] of ROLE_BY_ID) {
    db.insert(schema.users)
      .values({ username: `user${id}`, passwordHash: 'x', role, createdAt: new Date() })
      .run();
  }

  const moduleRef = await Test.createTestingModule({
    controllers: [ViewController],
    providers: [
      ConversationService,
      TaskService,
      TaskEventsService,
      { provide: DRIZZLE_INSTANCE, useValue: db },
    ],
  }).compile();

  conversations = moduleRef.get(ConversationService);
  tasks = moduleRef.get(TaskService);

  app = moduleRef.createNestApplication<NestExpressApplication>();
  app.setBaseViewsDir(VIEWS_DIR);
  app.setViewEngine('hbs');
  hbs.registerPartials(join(VIEWS_DIR, 'partials'));
  // Handlebars has no comparison operator; turn.hbs needs one to pick <img>
  // against <video>. Same registration as src/main.ts.
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

beforeEach(() => {
  connection.exec(
    'DELETE FROM generation_tasks; DELETE FROM assets; DELETE FROM messages; DELETE FROM conversations;',
  );
});

function get(path: string, userId: number | null = ALICE): Promise<globalThis.Response> {
  return fetch(`${origin}${path}`, {
    headers: userId === null ? {} : { [AUTH_HEADER]: String(userId) },
    redirect: 'manual',
  });
}

/** Moves a conversation's updatedAt into the past by the given number of ms. */
function backdateUpdatedAt(conversationId: number, ageMs: number): void {
  connection
    .prepare('UPDATE conversations SET updated_at = ? WHERE id = ?')
    .run(Date.now() - ageMs, conversationId);
}

describe('GET /login', () => {
  it('未登录时渲染登录表单', async () => {
    const response = await get('/login', null);
    expect(response.status).toBe(200);
    const html = await response.text();
    expect(html).toContain('login');
  });

  it('已登录时重定向到首页', async () => {
    const response = await get('/login');
    expect(response.status).toBe(302);
    expect(response.headers.get('location')).toBe('/');
  });

  it('带 error=1 时标记出错', async () => {
    const failed = await (await get('/login?error=1', null)).text();
    const clean = await (await get('/login', null)).text();
    // Rather than pinning the markup, assert the flag actually changes output.
    expect(failed).not.toBe(clean);
  });
});

describe('GET / — 会话选择', () => {
  it('未登录重定向到登录页', async () => {
    const response = await get('/', null);
    expect(response.status).toBe(302);
    expect(response.headers.get('location')).toBe('/login');
  });

  it('没有会话时仍然渲染页面外壳', async () => {
    const response = await get('/');
    expect(response.status).toBe(200);
    const html = await response.text();
    expect(html).toContain('id="stream"');
    expect(html).toContain('id="composer-form"');
  });

  it('默认打开最近更新的会话', async () => {
    const older = conversations.create(ALICE, '第一个');
    const newer = conversations.create(ALICE, '第二个');
    // States the primary sort key explicitly: both rows are created in the same
    // millisecond, so without backdating one, the id tie-break would decide the
    // order and this would stop testing updatedAt at all.
    backdateUpdatedAt(older, 60_000);

    const html = await (await get('/')).text();
    expect(html).toContain(`data-conversation-id="${newer}"`);
  });

  it('?c= 指定自己的会话时打开它', async () => {
    const first = conversations.create(ALICE, '第一个');
    conversations.create(ALICE, '第二个');

    const html = await (await get(`/?c=${first}`)).text();
    expect(html).toContain(`data-conversation-id="${first}"`);
  });

  it('?c= 指向别人的会话时回退到自己的第一个', async () => {
    const mine = conversations.create(ALICE, '我的');
    const theirs = conversations.create(BOB, 'bob 的');

    const html = await (await get(`/?c=${theirs}`)).text();
    expect(html).toContain(`data-conversation-id="${mine}"`);
    expect(html).not.toContain(`data-conversation-id="${theirs}"`);
  });

  it.each([['abc'], ['-1'], ['0'], ['9999']])('?c=%s 无法解析时回退', async (value) => {
    const mine = conversations.create(ALICE, '我的');
    const html = await (await get(`/?c=${value}`)).text();
    expect(html).toContain(`data-conversation-id="${mine}"`);
  });

  it('?c= 指向自己的空会话时不留旧内容', async () => {
    const empty = conversations.create(ALICE, '空会话');
    const other = conversations.create(ALICE, '有内容的');
    conversations.appendUserMessage(other, '早先的消息', 'chat');

    const html = await (await get(`/?c=${empty}`)).text();
    expect(html).toContain(`data-conversation-id="${empty}"`);
    expect(html).not.toContain('早先的消息');
  });
});

describe('GET / — 历史回放', () => {
  it('用户消息与助手正文都渲染出来', async () => {
    const conversationId = conversations.create(ALICE, 'q');
    conversations.appendUserMessage(conversationId, '画一只红隼', 'image');
    const assistant = conversations.createAssistantPlaceholder(conversationId, 'image');
    conversations.finalizeAssistant(assistant, {
      content: '已经画好了',
      reasoning: null,
      toolCalls: null,
    });

    const html = await (await get(`/?c=${conversationId}`)).text();
    expect(html).toContain('画一只红隼');
    expect(html).toContain('已经画好了');
  });

  it('助手正文按 markdown 渲染成 HTML', async () => {
    const conversationId = conversations.create(ALICE, 'q');
    const assistant = conversations.createAssistantPlaceholder(conversationId, 'chat');
    conversations.finalizeAssistant(assistant, {
      content: '**加粗**的说明',
      reasoning: null,
      toolCalls: null,
    });

    const html = await (await get(`/?c=${conversationId}`)).text();
    expect(html).toContain('<strong>加粗</strong>');
    // The template must inject it unescaped, or markdown would show as source.
    expect(html).not.toContain('**加粗**');
  });

  it('思考过程与工具调用一并回放', async () => {
    const conversationId = conversations.create(ALICE, 'q');
    const assistant = conversations.createAssistantPlaceholder(conversationId, 'image');
    conversations.finalizeAssistant(assistant, {
      content: '完成',
      reasoning: '先想构图',
      toolCalls: [{ name: 'generate_image', arguments: {}, ok: true, summary: '1 image' }],
    });

    const html = await (await get(`/?c=${conversationId}`)).text();
    expect(html).toContain('先想构图');
    expect(html).toContain('generate_image');
    expect(html).toContain('1 image');
  });

  it('图片资产渲染成 img，视频资产渲染成 video', async () => {
    const conversationId = conversations.create(ALICE, 'q');
    const assistant = conversations.createAssistantPlaceholder(conversationId, 'video');
    conversations.addAsset(assistant, {
      kind: 'image',
      filePath: '/app/storage/a.png',
      sourceUrl: 'https://provider.example/a.png',
      mime: 'image/png',
      bytes: 1,
    });
    const videoAsset = conversations.addAsset(assistant, {
      kind: 'video',
      filePath: '/app/storage/a.mp4',
      sourceUrl: 'https://provider.example/a.mp4',
      mime: 'video/mp4',
      bytes: 2,
    });

    const html = await (await get(`/?c=${conversationId}`)).text();
    expect(html).toContain('<img');
    expect(html).toContain('<video');
    // Renders the download endpoint, never the expiring provider URL.
    expect(html).toContain(`/api/assets/${videoAsset}/download`);
    expect(html).not.toContain('provider.example');
  });

  it('轮次索引由用户消息推出，空消息不参与', async () => {
    const conversationId = conversations.create(ALICE, 'q');
    conversations.appendUserMessage(conversationId, '第一个问题', 'chat');
    conversations.appendUserMessage(conversationId, '   ', 'chat');

    const html = await (await get(`/?c=${conversationId}`)).text();
    expect(html).toContain('turn-index-link');
    expect(html).toContain('第一个问题');
  });

  it('侧栏列出全部会话，且只在当前会话上标 is-active', async () => {
    const first = conversations.create(ALICE, '第一个');
    const second = conversations.create(ALICE, '第二个');
    backdateUpdatedAt(first, 60_000);

    const html = await (await get(`/?c=${first}`)).text();
    // Matched with the full class attribute: the mode buttons also carry
    // `is-active`, so a bare toContain('is-active') would pass vacuously.
    expect(html).toContain(`class="conversation-link is-active" href="/?c=${first}"`);
    expect(html).toContain(`class="conversation-link" href="/?c=${second}"`);
  });

  it('别人的会话不出现在侧栏', async () => {
    conversations.create(ALICE, '我的');
    conversations.create(BOB, 'bob 的秘密');

    const html = await (await get('/')).text();
    expect(html).not.toContain('bob 的秘密');
  });

  it('普通账号每行都带一个删除控件，且与链接同在一个列表项里', async () => {
    // main.ts addresses the button by this attribute from a delegated listener,
    // and reads the title off the sibling link — so the two have to be siblings.
    const id = conversations.create(ALICE, '要删的会话');

    const html = await (await get('/')).text();

    expect(html).toContain('data-can-delete-conversations="true"');
    expect(html).toContain(`data-delete-conversation="${id}"`);
    const item = /<li class="conversation-item">[\s\S]*?<\/li>/.exec(html);
    expect(item).not.toBeNull();
    expect(item?.[0]).toContain(`href="/?c=${id}"`);
    expect(item?.[0]).toContain(`data-delete-conversation="${id}"`);
  });

  it('超管不渲染删除控件 —— 删除只对普通账号开放', async () => {
    conversations.create(ROOT, '超管的会话');

    const html = await (await get('/', ROOT)).text();

    expect(html).toContain('data-can-delete-conversations="false"');
    expect(html).not.toContain('data-delete-conversation');
  });

  it('软删除的会话不出现在侧栏里', async () => {
    const hidden = conversations.create(ALICE, '已经删掉的会话');
    conversations.create(ALICE, '留着的会话');
    conversations.softDeleteConversations([hidden]);

    const html = await (await get('/')).text();

    expect(html).not.toContain('已经删掉的会话');
    expect(html).toContain('留着的会话');
  });

  it('进行中的任务渲染为等待提示', async () => {
    const conversationId = conversations.create(ALICE, 'q');
    const taskId = tasks.create({
      conversationId,
      messageId: null,
      kind: 'video',
      providerTaskId: 'p',
      model: 'm',
      prompt: 'p',
      params: {},
    });
    tasks.markProgress(taskId, 'running', 1);

    const html = await (await get(`/?c=${conversationId}`)).text();
    expect(html).toContain(`data-task-id="${taskId}"`);
    expect(html).toContain('渲染中');
  });
});

describe('GET / — 管理入口与超管的边界', () => {
  it('管理入口只挂给超管', async () => {
    const forRoot = await (await get('/', ROOT)).text();
    const forAlice = await (await get('/', ALICE)).text();

    expect(forRoot).toContain('href="/admin"');
    expect(forAlice).not.toContain('href="/admin"');
  });

  it('超管打开别人的会话也回退到自己的 —— 只读视图在 /admin 下', async () => {
    // The chat page is the caller's own workspace whatever the role: a super
    // admin reading someone else's conversation does it from the admin pages,
    // which render no composer.
    const mine = conversations.create(ROOT, '我的');
    const theirs = conversations.create(ALICE, 'alice 的');

    const html = await (await get(`/?c=${theirs}`, ROOT)).text();
    expect(html).toContain(`data-conversation-id="${mine}"`);
    expect(html).not.toContain(`data-conversation-id="${theirs}"`);
  });

  it('别人的会话不出现在超管的侧栏里', async () => {
    conversations.create(ROOT, '我的');
    conversations.create(ALICE, 'alice 的秘密');

    const html = await (await get('/', ROOT)).text();
    expect(html).not.toContain('alice 的秘密');
  });
});

describe('模板与客户端脚本的契约', () => {
  it('首屏 HTML 带上 main.ts 用 requireElement 取的全部元素', async () => {
    // Mirrors the requireElement() calls in web/scripts/main.ts. Renaming one of
    // these in a template breaks the page at runtime and nothing else catches it.
    const html = await (await get('/')).text();
    const requiredIds = [
      'stream',
      'composer-form',
      'prompt',
      'send',
      'new-conversation',
      'preview',
      'preview-image',
      'download-dialog',
      'download-format',
      'download-quality',
      'download-quality-value',
      'download-hint',
      'download-estimate',
      'download-confirm',
      'download-cancel',
    ];

    const missing = requiredIds.filter((id) => !html.includes(`id="${id}"`));
    expect(missing).toEqual([]);
  });

  it('模式选择器把四个模式都交给客户端', async () => {
    const html = await (await get('/')).text();
    for (const mode of ['auto', 'chat', 'image', 'video']) {
      expect(html).toContain(`data-mode="${mode}"`);
    }
  });

  it('data-conversation-id 挂在 body 上，脚本据此决定是否开流', async () => {
    const conversationId = conversations.create(ALICE, 'q');
    const html = await (await get(`/?c=${conversationId}`)).text();
    // The client reads document.body.dataset.conversationId, so it has to be on
    // <body>, not on some inner container.
    expect(/<body[^>]*data-conversation-id=/.test(html)).toBe(true);
  });

  it('脚本入口与样式表路径固定，模板只能按这两个名字引用', async () => {
    const html = await (await get('/')).text();
    expect(html).toContain('/assets/main.js');
    expect(html).toContain('/assets/main.css');
  });
});
