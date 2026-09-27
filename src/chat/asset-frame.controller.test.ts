import { Test } from '@nestjs/testing';
import type { NestExpressApplication } from '@nestjs/platform-express';
import Database from 'better-sqlite3';
import { drizzle } from 'drizzle-orm/better-sqlite3';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it } from 'vitest';
import { AssetFrameController } from '@/chat/asset-frame.controller';
import { buildSignedFrameUrl } from '@/common/signed-url';
import { ConversationService } from '@/conversation/conversation.service';
import { applySchema, DRIZZLE_INSTANCE } from '@/database/database.module';
import * as schema from '@/database/schema';

/**
 * This controller is deliberately NOT behind the session guard — the provider
 * fetches the frame itself for image-to-video, so the signed query string is the
 * entire authorization. Every test below is therefore about that one property:
 * a request without a valid, unexpired, correctly-scoped signature must not get
 * the bytes.
 *
 * The signing helpers are unit-tested in src/common/signed-url.test.ts; what is
 * checked here is that the controller actually consults them.
 */
const FRAME_TTL_MS = 2 * 60 * 60 * 1000;
const ENV_KEYS = ['SESSION_SECRET', 'PUBLIC_BASE_URL', 'STORAGE_DIR'] as const;
const saved = new Map<string, string | undefined>();

let app: NestExpressApplication;
let origin: string;
let connection: Database.Database;
let conversations: ConversationService;
let filesDir: string;

/** The signing helper returns an absolute public URL; the test needs its path. */
function framePath(assetId: number, now?: number): string {
  const url = new URL(
    now === undefined ? buildSignedFrameUrl(assetId) : buildSignedFrameUrl(assetId, now),
  );
  return `${url.pathname}${url.search}`;
}

async function bootstrap(): Promise<void> {
  connection = new Database(':memory:');
  applySchema(connection);
  const db = drizzle(connection, { schema });

  const moduleRef = await Test.createTestingModule({
    controllers: [AssetFrameController],
    providers: [ConversationService, { provide: DRIZZLE_INSTANCE, useValue: db }],
  }).compile();

  conversations = moduleRef.get(ConversationService);

  app = moduleRef.createNestApplication<NestExpressApplication>();
  await app.listen(0);
  origin = await app.getUrl();
}

beforeAll(async () => {
  filesDir = mkdtempSync(join(tmpdir(), 'kestrel-frame-'));
  await bootstrap();
});

afterAll(async () => {
  await app.close();
  connection.close();
  rmSync(filesDir, { recursive: true, force: true });
});

beforeEach(() => {
  for (const key of ENV_KEYS) {
    saved.set(key, process.env[key]);
  }
  process.env.SESSION_SECRET = 'frame-test-secret';
  process.env.PUBLIC_BASE_URL = 'https://frames.example.test';
  // Asset rows hold a file name; the controller resolves it against this.
  process.env.STORAGE_DIR = filesDir;

  connection.exec('DELETE FROM assets; DELETE FROM messages; DELETE FROM conversations;');
});

afterEach(() => {
  for (const key of ENV_KEYS) {
    const value = saved.get(key);
    if (value === undefined) {
      delete process.env[key];
    } else {
      process.env[key] = value;
    }
  }
  saved.clear();
});

let seeded = 0;

/** An image asset owned by nobody in particular — this endpoint does not care. */
function seedImageAsset(): number {
  // A fresh username per call: usernames are unique and `users` outlives the
  // per-test cleanup, so a fixed name collides on the second call.
  seeded += 1;
  const user = connection
    .prepare('INSERT INTO users (username, password_hash, created_at) VALUES (?, ?, ?)')
    .run(`u${seeded}`, 'x', Date.now());
  const conversationId = conversations.create(Number(user.lastInsertRowid), 'q');
  const messageId = conversations.createAssistantPlaceholder(conversationId, 'image');
  const filePath = join(filesDir, 'frame.png');
  writeFileSync(filePath, 'frame-bytes');
  return conversations.addAsset(messageId, {
    kind: 'image',
    filePath,
    sourceUrl: 'https://provider.example/expiring.png',
    mime: 'image/png',
    bytes: 11,
  });
}

function get(path: string): Promise<globalThis.Response> {
  // No cookie, no session: exactly how the provider fetches it.
  return fetch(`${origin}${path}`);
}

describe('GET /api/assets/:id/frame — 有效签名', () => {
  it('返回文件本身与正确的 Content-Type', async () => {
    const assetId = seedImageAsset();

    const response = await get(framePath(assetId));
    expect(response.status).toBe(200);
    expect(response.headers.get('content-type')).toContain('image/png');
    await expect(response.text()).resolves.toBe('frame-bytes');
  });

  it('不需要任何登录态 —— 供应商是匿名来取的', async () => {
    const assetId = seedImageAsset();
    const response = await get(framePath(assetId));
    // Would be 401/302 if a session guard were ever added to this controller.
    expect(response.status).toBe(200);
  });

  it('允许中间层短时缓存，但不公开', async () => {
    const assetId = seedImageAsset();
    const response = await get(framePath(assetId));
    expect(response.headers.get('cache-control')).toBe('private, max-age=600');
  });
});

describe('GET /api/assets/:id/frame — 签名必须有效', () => {
  it('过期签名返回 403', async () => {
    const assetId = seedImageAsset();
    const response = await get(framePath(assetId, Date.now() - 3 * FRAME_TTL_MS));
    expect(response.status).toBe(403);
  });

  it('篡改签名返回 403', async () => {
    const assetId = seedImageAsset();
    const tampered = framePath(assetId).replace(/sig=(.)/, (_m, c: string) =>
      `sig=${c === 'a' ? 'b' : 'a'}`,
    );
    expect((await get(tampered)).status).toBe(403);
  });

  it('把签名换到另一个资产上返回 403', async () => {
    const assetId = seedImageAsset();
    const other = seedImageAsset();
    const query = new URL(`https://x${framePath(assetId)}`).search;
    expect((await get(`/api/assets/${other}/frame${query}`)).status).toBe(403);
  });

  it('签名长度不对时返回 403 而不是 500', async () => {
    // timingSafeEqual throws on a length mismatch; the guard before it is what
    // keeps a malformed query string from becoming a server error.
    const assetId = seedImageAsset();
    const response = await get(`/api/assets/${assetId}/frame?exp=${Date.now() + 1000}&sig=short`);
    expect(response.status).toBe(403);
  });

  it('缺 sig 返回 403', async () => {
    const assetId = seedImageAsset();
    expect((await get(`/api/assets/${assetId}/frame?exp=${Date.now() + 1000}`)).status).toBe(403);
  });

  it('缺 exp 返回 403', async () => {
    const assetId = seedImageAsset();
    const sig = new URL(`https://x${framePath(assetId)}`).searchParams.get('sig') ?? '';
    expect((await get(`/api/assets/${assetId}/frame?sig=${sig}`)).status).toBe(403);
  });

  it('exp 不是数字时返回 403', async () => {
    const assetId = seedImageAsset();
    const query = framePath(assetId).replace(/exp=\d+/, 'exp=abc');
    expect((await get(query)).status).toBe(403);
  });

  it('换掉 SESSION_SECRET 后旧链接立即失效', async () => {
    const assetId = seedImageAsset();
    const path = framePath(assetId);

    process.env.SESSION_SECRET = 'rotated-secret';
    expect((await get(path)).status).toBe(403);
  });
});

describe('GET /api/assets/:id/frame — 资产不存在', () => {
  it('签名有效但资产不存在时返回 404', async () => {
    // Signed for an id that was never created: the signature is genuine, so
    // this is a 404 rather than a 403.
    expect((await get(framePath(4242))).status).toBe(404);
  });

  it('非法 id 返回 403', async () => {
    const assetId = seedImageAsset();
    const query = new URL(`https://x${framePath(assetId)}`).search;
    expect((await get(`/api/assets/abc/frame${query}`)).status).toBe(403);
  });
});
