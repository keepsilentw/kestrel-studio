import { Test } from '@nestjs/testing';
import Database from 'better-sqlite3';
import { drizzle } from 'drizzle-orm/better-sqlite3';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { hashToken, normalizeLabel, TokenService } from '@/auth/token.service';
import {
  applySchema,
  DRIZZLE_INSTANCE,
  SQLITE_CONNECTION,
} from '@/database/database.module';
import * as schema from '@/database/schema';

interface Harness {
  tokens: TokenService;
  raw: Database.Database;
  userId: number;
}

let harness: Harness;

async function buildHarness(): Promise<Harness> {
  const connection = new Database(':memory:');
  applySchema(connection);
  // The real store owns this table and creates it at boot; tests create it by hand.
  connection.exec(
    'CREATE TABLE sessions (sid TEXT NOT NULL PRIMARY KEY, sess TEXT NOT NULL, expire TEXT NOT NULL)',
  );
  const db = drizzle(connection, { schema });
  const result = db
    .insert(schema.users)
    .values({ username: 'alice', passwordHash: 'h', role: 'user', createdAt: new Date() })
    .run();

  const moduleRef = await Test.createTestingModule({
    providers: [
      TokenService,
      { provide: DRIZZLE_INSTANCE, useValue: db },
      { provide: SQLITE_CONNECTION, useValue: connection },
    ],
  }).compile();

  return { tokens: moduleRef.get(TokenService), raw: connection, userId: Number(result.lastInsertRowid) };
}

beforeEach(async () => {
  harness = await buildHarness();
});

afterEach(() => {
  harness.raw.close();
});

describe('TokenService', () => {
  it('签发的明文只在返回里，库里只有摘要，且能解析回用户', () => {
    const issued = harness.tokens.issue(harness.userId, '  手机  ');
    expect(issued.token.startsWith('kst_')).toBe(true);
    expect(issued.label).toBe('手机');
    expect(harness.tokens.resolveBearer(issued.token)).toBe(harness.userId);

    const stored = harness.raw
      .prepare('SELECT token_hash AS hash FROM api_tokens WHERE id = ?')
      .get(issued.id) as { hash: string };
    expect(stored.hash).toBe(hashToken(issued.token));
    expect(stored.hash).not.toBe(issued.token);
  });

  it('撤销后不再解析，也不再出现在列表里', () => {
    const issued = harness.tokens.issue(harness.userId, 'laptop');
    expect(harness.tokens.list(harness.userId)).toHaveLength(1);

    expect(harness.tokens.revoke(harness.userId, issued.id)).toBe(true);
    expect(harness.tokens.revoke(harness.userId, issued.id)).toBe(false);
    expect(harness.tokens.resolveBearer(issued.token)).toBeNull();
    expect(harness.tokens.list(harness.userId)).toEqual([]);
  });

  it('别人的令牌读作不存在', () => {
    const issued = harness.tokens.issue(harness.userId, 'laptop');
    expect(harness.tokens.revoke(harness.userId + 1, issued.id)).toBe(false);
    expect(harness.tokens.resolveBearer(issued.token)).toBe(harness.userId);
  });

  it('会话 sid 也能当 Bearer：passport 的嵌套形状与 ISO 过期时间', () => {
    harness.raw
      .prepare('INSERT INTO sessions (sid, sess, expire) VALUES (?, ?, ?)')
      .run(
        'sid-ok',
        JSON.stringify({ passport: { user: harness.userId } }),
        new Date(Date.now() + 60_000).toISOString(),
      );
    harness.raw
      .prepare('INSERT INTO sessions (sid, sess, expire) VALUES (?, ?, ?)')
      .run(
        'sid-expired',
        JSON.stringify({ passport: { user: harness.userId } }),
        new Date(Date.now() - 60_000).toISOString(),
      );

    expect(harness.tokens.resolveBearer('sid-ok')).toBe(harness.userId);
    expect(harness.tokens.resolveBearer('sid-expired')).toBeNull();
    expect(harness.tokens.resolveBearer('sid-unknown')).toBeNull();
  });

  it('空标签回落到默认名，超长标签截到 64 字', () => {
    expect(normalizeLabel('   ')).toBe('未命名设备');
    expect(normalizeLabel('x'.repeat(80))).toHaveLength(64);
  });
});
