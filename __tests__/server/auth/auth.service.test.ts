import { Test } from '@nestjs/testing';
import { hash } from 'bcryptjs';
import Database from 'better-sqlite3';
import { drizzle } from 'drizzle-orm/better-sqlite3';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { AuthService } from '@/auth/auth.service';
import type { Role } from '@/auth/roles';
import { applySchema, DRIZZLE_INSTANCE } from '@/database/database.module';
import * as schema from '@/database/schema';

/**
 * Uses a low bcrypt cost factor: `compare` reads the cost from the stored hash,
 * so a real round-trip is exercised without paying ten rounds per test.
 */
const COST = 4;

interface Harness {
  auth: AuthService;
  seedUser(username: string, password: string, role?: Role): Promise<number>;
  raw: Database.Database;
}

let harness: Harness;

async function buildHarness(): Promise<Harness> {
  const connection = new Database(':memory:');
  applySchema(connection);
  const db = drizzle(connection, { schema });

  const moduleRef = await Test.createTestingModule({
    providers: [AuthService, { provide: DRIZZLE_INSTANCE, useValue: db }],
  }).compile();

  return {
    auth: moduleRef.get(AuthService),
    raw: connection,
    async seedUser(username: string, password: string, role: Role = 'user'): Promise<number> {
      const result = db
        .insert(schema.users)
        .values({ username, passwordHash: await hash(password, COST), role, createdAt: new Date() })
        .run();
      return Number(result.lastInsertRowid);
    },
  };
}

beforeEach(async () => {
  harness = await buildHarness();
});

afterEach(() => {
  harness.raw.close();
});

describe('AuthService.validate', () => {
  it('用户名与密码都对时返回主体', async () => {
    const id = await harness.seedUser('alice', '正确的密码');

    await expect(harness.auth.validate('alice', '正确的密码')).resolves.toEqual({
      id,
      username: 'alice',
      role: 'user',
    });
  });

  it('角色从库里带进主体', async () => {
    const id = await harness.seedUser('root', '正确的密码', 'super');

    await expect(harness.auth.validate('root', '正确的密码')).resolves.toEqual({
      id,
      username: 'root',
      role: 'super',
    });
  });

  it('密码错时返回 null', async () => {
    await harness.seedUser('alice', '正确的密码');
    await expect(harness.auth.validate('alice', '错误的密码')).resolves.toBeNull();
  });

  it('用户不存在时返回 null', async () => {
    await expect(harness.auth.validate('nobody', 'whatever')).resolves.toBeNull();
  });

  it('用户名大小写敏感', async () => {
    await harness.seedUser('alice', 'p');
    await expect(harness.auth.validate('Alice', 'p')).resolves.toBeNull();
  });

  it('空密码不会意外通过', async () => {
    await harness.seedUser('alice', 'p');
    await expect(harness.auth.validate('alice', '')).resolves.toBeNull();
  });

  it('返回的主体不带密码哈希', async () => {
    await harness.seedUser('alice', 'p');
    const user = await harness.auth.validate('alice', 'p');
    expect(user).not.toBeNull();
    expect(Object.keys(user ?? {})).toEqual(['id', 'username', 'role']);
  });

  it('两个用户各自校验，不串号', async () => {
    const alice = await harness.seedUser('alice', 'alice-pw');
    const bob = await harness.seedUser('bob', 'bob-pw');

    await expect(harness.auth.validate('alice', 'alice-pw')).resolves.toEqual({
      id: alice,
      username: 'alice',
      role: 'user',
    });
    await expect(harness.auth.validate('bob', 'bob-pw')).resolves.toEqual({
      id: bob,
      username: 'bob',
      role: 'user',
    });
    await expect(harness.auth.validate('alice', 'bob-pw')).resolves.toBeNull();
  });
});

describe('AuthService.findById', () => {
  it('按 id 还原主体', async () => {
    const id = await harness.seedUser('alice', 'p');
    expect(harness.auth.findById(id)).toEqual({ id, username: 'alice', role: 'user' });
  });

  it('角色也一并还原 —— 会话重建后权限不丢', async () => {
    const id = await harness.seedUser('root', 'p', 'super');
    expect(harness.auth.findById(id)).toEqual({ id, username: 'root', role: 'super' });
  });

  it('不存在时返回 null', () => {
    expect(harness.auth.findById(9999)).toBeNull();
  });
});
