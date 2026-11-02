import { Inject, Injectable } from '@nestjs/common';
import { hash } from 'bcryptjs';
import { asc, count, eq } from 'drizzle-orm';
import { ROLE_LABEL, parseRole, type Role, type Viewer } from '@/auth/roles';
import { ConversationService } from '@/conversation/conversation.service';
import { DRIZZLE_INSTANCE, type DrizzleDb } from '@/database/database.module';
import { assets, conversations, messages, users, type User } from '@/database/schema';

/** Accounts are typed by hand on the admin page, so keep them URL-safe. */
const USERNAME_PATTERN = /^[a-zA-Z0-9_-]{3,32}$/;
const MIN_PASSWORD_LENGTH = 6;
const MAX_PASSWORD_LENGTH = 128;

export interface AccountView {
  id: number;
  username: string;
  role: Role;
  roleLabel: string;
  createdAt: number;
  conversationCount: number;
  assetCount: number;
}

export type CreateAccountResult =
  | { ok: true; id: number }
  | { ok: false; reason: 'username' | 'password' | 'duplicate' };

export type ResetPasswordResult = { ok: true } | { ok: false; reason: 'password' | 'missing' };

export type RemoveAccountResult =
  | { ok: true; conversations: number; assets: number }
  | { ok: false; reason: 'missing' | 'self' | 'last-super' };

export function isAcceptablePassword(password: string): boolean {
  return (
    password.trim().length >= MIN_PASSWORD_LENGTH &&
    password.length <= MAX_PASSWORD_LENGTH
  );
}

function toAccountView(
  row: User,
  conversationCount: number,
  assetCount: number,
): AccountView {
  const role = parseRole(row.role);
  return {
    id: row.id,
    username: row.username,
    role,
    roleLabel: ROLE_LABEL[role],
    createdAt: row.createdAt.getTime(),
    conversationCount,
    assetCount,
  };
}

@Injectable()
export class AdminService {
  constructor(
    @Inject(DRIZZLE_INSTANCE) private readonly db: DrizzleDb,
    private readonly conversations: ConversationService,
  ) {}

  list(): AccountView[] {
    const conversationCounts = this.groupedConversationCounts();
    const assetCounts = this.groupedAssetCounts();
    return this.db
      .select()
      .from(users)
      .orderBy(asc(users.id))
      .all()
      .map((row) =>
        toAccountView(row, conversationCounts.get(row.id) ?? 0, assetCounts.get(row.id) ?? 0),
      );
  }

  find(id: number): AccountView | null {
    const row = this.db.select().from(users).where(eq(users.id, id)).get();
    if (row === undefined) {
      return null;
    }
    return toAccountView(
      row,
      this.groupedConversationCounts().get(id) ?? 0,
      this.groupedAssetCounts().get(id) ?? 0,
    );
  }

  /** New accounts are always plain users; promotion is not part of this surface. */
  async create(username: string, password: string): Promise<CreateAccountResult> {
    const name = username.trim();
    if (!USERNAME_PATTERN.test(name)) {
      return { ok: false, reason: 'username' };
    }
    if (!isAcceptablePassword(password)) {
      return { ok: false, reason: 'password' };
    }

    // Hash before checking: the check and the insert then run in one synchronous
    // block, so nothing can take the name in between.
    const passwordHash = await hash(password, 10);
    const existing = this.db
      .select({ id: users.id })
      .from(users)
      .where(eq(users.username, name))
      .get();
    if (existing !== undefined) {
      return { ok: false, reason: 'duplicate' };
    }

    const result = this.db
      .insert(users)
      .values({ username: name, passwordHash, role: 'user', createdAt: new Date() })
      .run();
    return { ok: true, id: Number(result.lastInsertRowid) };
  }

  async resetPassword(id: number, password: string): Promise<ResetPasswordResult> {
    if (!isAcceptablePassword(password)) {
      return { ok: false, reason: 'password' };
    }
    const passwordHash = await hash(password, 10);
    const result = this.db.update(users).set({ passwordHash }).where(eq(users.id, id)).run();
    return result.changes > 0 ? { ok: true } : { ok: false, reason: 'missing' };
  }

  /**
   * Removes an account and everything it owns: conversations, messages, tasks,
   * asset rows and the mirrored files.
   *
   * The conversation cascade is ConversationService's (shared with deleting a
   * single conversation); the account row goes after it, so a failure there
   * leaves an account with no conversations rather than orphaned rows.
   *
   * Refuses to remove the acting account (which would end the session mid-flight)
   * and the last super admin (which would leave nobody able to manage accounts).
   */
  async remove(id: number, actor: Viewer): Promise<RemoveAccountResult> {
    if (id === actor.id) {
      return { ok: false, reason: 'self' };
    }

    const target = this.db.select().from(users).where(eq(users.id, id)).get();
    if (target === undefined) {
      return { ok: false, reason: 'missing' };
    }
    if (parseRole(target.role) === 'super' && this.superAdminCount() <= 1) {
      return { ok: false, reason: 'last-super' };
    }

    const conversationIds = this.db
      .select({ id: conversations.id })
      .from(conversations)
      .where(eq(conversations.userId, id))
      .all()
      .map((row) => row.id);

    const removed = await this.conversations.deleteConversations(conversationIds);
    this.db.delete(users).where(eq(users.id, id)).run();

    return { ok: true, conversations: removed.conversations, assets: removed.assets };
  }

  private groupedConversationCounts(): Map<number, number> {
    const rows = this.db
      .select({ userId: conversations.userId, total: count() })
      .from(conversations)
      .groupBy(conversations.userId)
      .all();
    return new Map(rows.map((row) => [row.userId, row.total]));
  }

  private groupedAssetCounts(): Map<number, number> {
    const rows = this.db
      .select({ userId: conversations.userId, total: count() })
      .from(assets)
      .innerJoin(messages, eq(assets.messageId, messages.id))
      .innerJoin(conversations, eq(messages.conversationId, conversations.id))
      .groupBy(conversations.userId)
      .all();
    return new Map(rows.map((row) => [row.userId, row.total]));
  }

  private superAdminCount(): number {
    return this.db
      .select({ id: users.id })
      .from(users)
      .where(eq(users.role, 'super'))
      .all().length;
  }
}
