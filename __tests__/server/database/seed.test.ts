import { Test } from '@nestjs/testing';
import { hash } from 'bcryptjs';
import Database from 'better-sqlite3';
import { drizzle } from 'drizzle-orm/better-sqlite3';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { AuthService } from '@/auth/auth.service';
import { applySchema, DRIZZLE_INSTANCE } from '@/database/database.module';
import type { DrizzleDb } from '@/database/database.module';
import * as schema from '@/database/schema';
import type { SeedAccount } from '@/config/configuration';
import { DEFAULT_ADMIN_PASSWORD, DEFAULT_ADMIN_USERNAME, ensureAdminUser, ensureSuperAdmin } from '@/database/seed';

/**
 * The super admin comes from the environment, so the test supplies it the same
 * way `loadConfig()` would. Nothing here is a credential anyone can reuse:
 * these accounts only ever exist in an in-memory database.
 */
const SUPER_ADMIN: SeedAccount = { username: 'operator-root', password: 'a-long-one' };

/**
 * The bootstrap accounts are what make a fresh deployment reachable, and the
 * idempotence is what stops a restart from resetting a password the operator
 * changed. Both are worth pinning: the failure mode is either a locked-out
 * deployment or a silently reopened default credential.
 */
interface Harness {
  db: DrizzleDb;
  auth: AuthService;
  adminCount(): number;
  passwordHashOf(username: string): string | null;
  roleOf(username: string): string | null;
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
    db,
    auth: moduleRef.get(AuthService),
    raw: connection,
    adminCount(): number {
      const row = connection
        .prepare('SELECT COUNT(*) AS n FROM users WHERE username = ?')
        .get(DEFAULT_ADMIN_USERNAME) as { n: number };
      return row.n;
    },
    passwordHashOf(username: string): string | null {
      const row = connection
        .prepare('SELECT password_hash AS hash FROM users WHERE username = ?')
        .get(username) as { hash: string } | undefined;
      return row?.hash ?? null;
    },
    roleOf(username: string): string | null {
      const row = connection
        .prepare('SELECT role FROM users WHERE username = ?')
        .get(username) as { role: string } | undefined;
      return row?.role ?? null;
    },
  };
}

beforeEach(async () => {
  harness = await buildHarness();
});

afterEach(() => {
  harness.raw.close();
});

describe('ensureAdminUser', () => {
  it('首次调用建出账号并返回 true', async () => {
    await expect(ensureAdminUser(harness.db)).resolves.toBe(true);
    expect(harness.adminCount()).toBe(1);
  });

  it('密码以哈希入库，不存明文', async () => {
    await ensureAdminUser(harness.db);
    const stored = harness.passwordHashOf(DEFAULT_ADMIN_USERNAME);
    expect(stored).not.toBeNull();
    expect(stored).not.toBe(DEFAULT_ADMIN_PASSWORD);
    // bcrypt hashes are $2a$/$2b$ prefixed and 60 characters long.
    expect(stored ?? '').toMatch(/^\$2[aby]\$\d{2}\$.{53}$/);
  });

  it('默认账号能真的登录', async () => {
    // Cross-checks the exported constants against the hashing path: a mismatch
    // here is a deployment nobody can get into.
    await ensureAdminUser(harness.db);
    const user = await harness.auth.validate(DEFAULT_ADMIN_USERNAME, DEFAULT_ADMIN_PASSWORD);
    expect(user).not.toBeNull();
    expect(user?.username).toBe(DEFAULT_ADMIN_USERNAME);
  });

  it('已经存在时返回 false，不再插入', async () => {
    await ensureAdminUser(harness.db);
    await expect(ensureAdminUser(harness.db)).resolves.toBe(false);
    expect(harness.adminCount()).toBe(1);
  });

  it('不重置已改过的密码', async () => {
    // A restart must not reopen the default credential.
    await ensureAdminUser(harness.db);
    const changed = await hash('operator-changed-this', 4);
    harness.raw
      .prepare('UPDATE users SET password_hash = ? WHERE username = ?')
      .run(changed, DEFAULT_ADMIN_USERNAME);

    await expect(ensureAdminUser(harness.db)).resolves.toBe(false);

    expect(harness.passwordHashOf(DEFAULT_ADMIN_USERNAME)).toBe(changed);
    await expect(
      harness.auth.validate(DEFAULT_ADMIN_USERNAME, DEFAULT_ADMIN_PASSWORD),
    ).resolves.toBeNull();
    await expect(
      harness.auth.validate(DEFAULT_ADMIN_USERNAME, 'operator-changed-this'),
    ).resolves.not.toBeNull();
  });

  it('不影响其它账号', async () => {
    harness.db
      .insert(schema.users)
      .values({ username: 'alice', passwordHash: 'x', role: 'user', createdAt: new Date() })
      .run();

    await ensureAdminUser(harness.db);

    expect(harness.adminCount()).toBe(1);
    const total = harness.raw.prepare('SELECT COUNT(*) AS n FROM users').get() as { n: number };
    expect(total.n).toBe(2);
  });

  it('建出的是普通账号，不是超管', async () => {
    await ensureAdminUser(harness.db);
    expect(harness.roleOf(DEFAULT_ADMIN_USERNAME)).toBe('user');
  });
});

describe('ensureSuperAdmin', () => {
  it('首次调用建出账号并返回 true', async () => {
    await expect(ensureSuperAdmin(harness.db, SUPER_ADMIN)).resolves.toBe(true);
    expect(harness.roleOf(SUPER_ADMIN.username)).toBe('super');
  });

  it('env 注入的凭据能真的登录', async () => {
    // Same cross-check as the default account: a mismatch between the injected
    // credentials and the hashing path is a super admin nobody can get into.
    await ensureSuperAdmin(harness.db, SUPER_ADMIN);
    const user = await harness.auth.validate(SUPER_ADMIN.username, SUPER_ADMIN.password);
    expect(user?.username).toBe(SUPER_ADMIN.username);
    expect(user?.role).toBe('super');
  });

  it('已经存在时返回 false，且不重置已改过的密码', async () => {
    await ensureSuperAdmin(harness.db, SUPER_ADMIN);
    const changed = await hash('rotated-by-the-operator', 4);
    harness.raw
      .prepare('UPDATE users SET password_hash = ? WHERE username = ?')
      .run(changed, SUPER_ADMIN.username);

    await expect(ensureSuperAdmin(harness.db, SUPER_ADMIN)).resolves.toBe(false);
    expect(harness.passwordHashOf(SUPER_ADMIN.username)).toBe(changed);
  });

  it('不把已存在的同名普通账号提升为超管', async () => {
    // Promotion is a deliberate act, not a side effect of a restart.
    harness.db
      .insert(schema.users)
      .values({ username: SUPER_ADMIN.username, passwordHash: 'x', role: 'user', createdAt: new Date() })
      .run();

    await expect(ensureSuperAdmin(harness.db, SUPER_ADMIN)).resolves.toBe(false);
    expect(harness.roleOf(SUPER_ADMIN.username)).toBe('user');
  });

  it('凭据缺省时什么都不建', async () => {
    // A deployment with no SUPER_ADMIN_* vars must not invent a super admin,
    // and must not leave a guessable one behind either.
    await expect(ensureSuperAdmin(harness.db, null)).resolves.toBe(false);
    const total = harness.raw.prepare('SELECT COUNT(*) AS n FROM users').get() as { n: number };
    expect(total.n).toBe(0);
    expect(harness.roleOf('operator-root')).toBeNull();
  });

  it('与默认账号各自独立', async () => {
    await ensureAdminUser(harness.db);
    await ensureSuperAdmin(harness.db, SUPER_ADMIN);

    expect(harness.roleOf(DEFAULT_ADMIN_USERNAME)).toBe('user');
    expect(harness.roleOf(SUPER_ADMIN.username)).toBe('super');
  });
});
