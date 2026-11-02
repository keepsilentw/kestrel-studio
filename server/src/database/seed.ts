import { hash } from 'bcryptjs';
import { eq } from 'drizzle-orm';
import type { Role } from '@/auth/roles';
import type { SeedAccount } from '@/config/configuration';
import type { DrizzleDb } from './database.module';
import { users } from './schema';

/**
 * The plain account kept for first-run local access; no privileges beyond its
 * own data. It is the only credential the repository ships, and it is worth
 * changing before anything leaves the laptop.
 */
export const DEFAULT_ADMIN_USERNAME = 'admin';
export const DEFAULT_ADMIN_PASSWORD = '123456';

/**
 * Idempotent: creates an account only when it is missing, so a restart never
 * resets a password the user has changed.
 */
async function ensureUser(
  db: DrizzleDb,
  username: string,
  password: string,
  role: Role,
): Promise<boolean> {
  const existing = db
    .select({ id: users.id })
    .from(users)
    .where(eq(users.username, username))
    .get();

  if (existing !== undefined) {
    return false;
  }

  db.insert(users)
    .values({
      username,
      passwordHash: await hash(password, 10),
      role,
      createdAt: new Date(),
    })
    .run();

  return true;
}

export async function ensureAdminUser(db: DrizzleDb): Promise<boolean> {
  return ensureUser(db, DEFAULT_ADMIN_USERNAME, DEFAULT_ADMIN_PASSWORD, 'user');
}

/**
 * The super admin is not a constant: its credentials arrive from the
 * environment (`SUPER_ADMIN_USERNAME` / `SUPER_ADMIN_PASSWORD`, read in
 * `loadConfig()`), so the repository alone never describes a working login for
 * a deployed instance. With either variable absent nothing is seeded and
 * `/admin` stays unreachable.
 *
 * Like the default account, the password is applied **only when the row is
 * created** — a later change through /admin (or a restart) never overwrites it.
 * Rotate it from the admin page, not by editing the environment file.
 */
export async function ensureSuperAdmin(
  db: DrizzleDb,
  account: SeedAccount | null,
): Promise<boolean> {
  if (account === null) {
    return false;
  }
  return ensureUser(db, account.username, account.password, 'super');
}
