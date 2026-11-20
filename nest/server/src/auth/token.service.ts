import { Inject, Injectable } from '@nestjs/common';
import type Database from 'better-sqlite3';
import { createHash, randomBytes } from 'node:crypto';
import { and, desc, eq, gt, isNull, or } from 'drizzle-orm';
import { loadConfig } from '@/config/configuration';
import {
  DRIZZLE_INSTANCE,
  SQLITE_CONNECTION,
  type DrizzleDb,
} from '@/database/database.module';
import { apiTokens } from '@/database/schema';

/** Every device token carries this prefix, so it cannot be confused with a session sid. */
const TOKEN_PREFIX = 'kst_';
const SEEN_THROTTLE_MS = 60 * 60 * 1000;

export interface IssuedToken {
  id: number;
  label: string;
  token: string;
  createdAt: number;
  expiresAt: number | null;
}

export interface TokenSummary {
  id: number;
  label: string;
  createdAt: number;
  lastSeenAt: number | null;
  expiresAt: number | null;
}

/** `null` when the lifetime is 0: that token never expires. */
function expiryFrom(createdAt: number, ttlDays: number): Date | null {
  return ttlDays > 0 ? new Date(createdAt + ttlDays * 86_400_000) : null;
}

/** A blank label falls back to a human-readable default; a long one is capped, not rejected. */
export function normalizeLabel(raw: string): string {
  const trimmed = raw.trim();
  if (trimmed.length === 0) {
    return '未命名设备';
  }
  return [...trimmed].slice(0, 64).join('');
}

/** Only the digest is stored; the plaintext exists only in the issuing response. */
export function hashToken(token: string): string {
  return createHash('sha256').update(token, 'utf8').digest('hex');
}

/** Accepts the ISO-8601 text the session store writes and epoch-millis text. */
function parseExpire(raw: string): number | null {
  const trimmed = raw.trim();
  if (/^\d+$/.test(trimmed)) {
    return Number(trimmed);
  }
  const parsed = Date.parse(trimmed);
  return Number.isNaN(parsed) ? null : parsed;
}

@Injectable()
export class TokenService {
  constructor(
    @Inject(DRIZZLE_INSTANCE) private readonly db: DrizzleDb,
    @Inject(SQLITE_CONNECTION) private readonly connection: Database.Database,
  ) {}

  issue(
    userId: number,
    label: string,
    ttlDays: number = loadConfig().tokenTtlDays,
  ): IssuedToken {
    const token = `${TOKEN_PREFIX}${randomBytes(24).toString('hex')}`;
    const createdAt = Date.now();
    const normalized = normalizeLabel(label);
    const expiresAt = expiryFrom(createdAt, ttlDays);
    const result = this.db
      .insert(apiTokens)
      .values({
        userId,
        label: normalized,
        tokenHash: hashToken(token),
        createdAt: new Date(createdAt),
        expiresAt,
      })
      .run();
    return {
      id: Number(result.lastInsertRowid),
      label: normalized,
      token,
      createdAt,
      expiresAt: expiresAt === null ? null : expiresAt.getTime(),
    };
  }

  /**
   * Resolves either credential kind a Bearer header may carry: a device token from
   * `api_tokens`, or the session sid returned by `POST /api/v1/session`.
   */
  resolveBearer(token: string): number | null {
    const deviceUser = this.resolveDevice(token);
    if (deviceUser !== null) {
      return deviceUser;
    }
    return this.principalFromSession(token);
  }

  list(userId: number): TokenSummary[] {
    return this.db
      .select({
        id: apiTokens.id,
        label: apiTokens.label,
        createdAt: apiTokens.createdAt,
        lastSeenAt: apiTokens.lastSeenAt,
        expiresAt: apiTokens.expiresAt,
      })
      .from(apiTokens)
      .where(
        and(
          eq(apiTokens.userId, userId),
          isNull(apiTokens.revokedAt),
          // Expired rows are dead weight: like revoked ones, they are not "active".
          or(isNull(apiTokens.expiresAt), gt(apiTokens.expiresAt, new Date())),
        ),
      )
      .orderBy(desc(apiTokens.id))
      .all()
      .map((row) => ({
        id: row.id,
        label: row.label,
        createdAt: row.createdAt.getTime(),
        lastSeenAt: row.lastSeenAt === null ? null : row.lastSeenAt.getTime(),
        expiresAt: row.expiresAt === null ? null : row.expiresAt.getTime(),
      }));
  }

  /** Owner-scoped: another account's id reads as `false`, which the handler turns into 404. */
  revoke(userId: number, id: number): boolean {
    const result = this.db
      .update(apiTokens)
      .set({ revokedAt: new Date() })
      .where(
        and(
          eq(apiTokens.id, id),
          eq(apiTokens.userId, userId),
          isNull(apiTokens.revokedAt),
        ),
      )
      .run();
    return result.changes === 1;
  }

  private resolveDevice(token: string): number | null {
    const row = this.db
      .select({
        id: apiTokens.id,
        userId: apiTokens.userId,
        lastSeenAt: apiTokens.lastSeenAt,
        expiresAt: apiTokens.expiresAt,
      })
      .from(apiTokens)
      .where(and(eq(apiTokens.tokenHash, hashToken(token)), isNull(apiTokens.revokedAt)))
      .get();
    if (row === undefined) {
      return null;
    }
    const now = Date.now();
    if (row.expiresAt !== null && row.expiresAt.getTime() <= now) {
      // Expired reads exactly like revoked: 401 and no detail.
      return null;
    }
    const lastSeen = row.lastSeenAt === null ? null : row.lastSeenAt.getTime();
    if (lastSeen === null || now - lastSeen >= SEEN_THROTTLE_MS) {
      this.db
        .update(apiTokens)
        .set({ lastSeenAt: new Date(now) })
        .where(eq(apiTokens.id, row.id))
        .run();
    }
    return row.userId;
  }

  /** Reads the store's own row shape: `{user}` here, `{passport:{user}}` from passport. */
  private principalFromSession(sid: string): number | null {
    const row = this.connection
      .prepare('SELECT sess, CAST(expire AS TEXT) AS expire FROM sessions WHERE sid = ?')
      .get(sid) as { sess: string; expire: string } | undefined;
    if (row === undefined) {
      return null;
    }
    const expire = parseExpire(row.expire);
    if (expire === null || expire < Date.now()) {
      return null;
    }
    try {
      const parsed: unknown = JSON.parse(row.sess);
      if (typeof parsed !== 'object' || parsed === null) {
        return null;
      }
      const root = parsed as { user?: unknown; passport?: { user?: unknown } };
      const id = root.user ?? root.passport?.user;
      return typeof id === 'number' ? id : null;
    } catch {
      return null;
    }
  }
}
