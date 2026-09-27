import { Inject, Injectable } from '@nestjs/common';
import { and, asc, desc, eq, inArray, isNull, or, type SQL } from 'drizzle-orm';
import type { ResponsesInputItem } from '@/bailian/responses-client';
import type { Mode } from '@/agent/mode';
import { isSuperAdmin, type Viewer } from '@/auth/roles';
import { DRIZZLE_INSTANCE, type DrizzleDb } from '@/database/database.module';
import { assets, conversations, generationTasks, messages, users } from '@/database/schema';
import { removeAssetFiles, resolveAssetPath, storedAssetName } from '@/media/asset-path';
import type { StoredAsset } from '@/media/media.service';

export interface ToolCallRecord {
  name: string;
  arguments: Record<string, unknown>;
  ok: boolean;
  summary: string;
}

export interface AssetView {
  id: number;
  kind: 'image' | 'video';
  /** Download endpoint, not the on-disk path. */
  url: string;
  mime: string;
  bytes: number;
}

export interface MessageView {
  id: number;
  role: 'user' | 'assistant';
  content: string;
  reasoning: string | null;
  toolCalls: ToolCallRecord[] | null;
  mode: Mode | null;
  createdAt: number;
  assets: AssetView[];
}

export interface ConversationSummary {
  id: number;
  title: string;
  updatedAt: number;
}

/** A conversation plus whose it is, for the super admin's site-wide list. */
export interface AdminConversationSummary extends ConversationSummary {
  ownerId: number;
  ownerName: string;
  /** Set when the owner has deleted it; the super admin still reads it. */
  deletedAt: number | null;
}

/**
 * The read-visibility rule, expressed once for every query that needs it.
 *
 * A normal account reads only its own, and not the ones it has deleted. A super
 * admin reads every normal account's as well — never another super admin's, so a
 * privileged account's own sessions stay private (docs/architecture.md §12) — and
 * the soft-deleted ones stay readable for it, which is what makes the delete a
 * hide rather than a loss. Requires `users` to be joined on
 * `users.id = conversations.user_id`.
 */
function readableBy(viewer: Viewer): SQL<unknown> | undefined {
  const live = and(eq(conversations.userId, viewer.id), isNull(conversations.deletedAt));
  return isSuperAdmin(viewer) ? or(live, eq(users.role, 'user')) : live;
}

/** Shared by the admin list and the admin single-conversation lookup. */
const ADMIN_SUMMARY_COLUMNS = {
  id: conversations.id,
  title: conversations.title,
  updatedAt: conversations.updatedAt,
  deletedAt: conversations.deletedAt,
  ownerId: users.id,
  ownerName: users.username,
};

function toAdminSummary(row: {
  id: number;
  title: string;
  updatedAt: Date;
  deletedAt: Date | null;
  ownerId: number;
  ownerName: string;
}): AdminConversationSummary {
  return {
    id: row.id,
    title: row.title,
    updatedAt: row.updatedAt.getTime(),
    deletedAt: row.deletedAt === null ? null : row.deletedAt.getTime(),
    ownerId: row.ownerId,
    ownerName: row.ownerName,
  };
}

const MAX_TITLE_LENGTH = 30;

/**
 * Where the browser fetches an asset from. Single source of the format: it is
 * also what the turn stream and the voice socket hand to the client.
 */
export function assetUrl(assetId: number): string {
  return `/api/assets/${assetId}/download`;
}

function toAssetView(row: { id: number; kind: 'image' | 'video'; mime: string; bytes: number }): AssetView {
  return {
    id: row.id,
    kind: row.kind,
    url: assetUrl(row.id),
    mime: row.mime,
    bytes: row.bytes,
  };
}

function parseToolCalls(raw: string | null): ToolCallRecord[] | null {
  if (raw === null || raw.length === 0) {
    return null;
  }
  try {
    const parsed: unknown = JSON.parse(raw);
    return Array.isArray(parsed) ? (parsed as ToolCallRecord[]) : null;
  } catch {
    return null;
  }
}

@Injectable()
export class ConversationService {
  constructor(@Inject(DRIZZLE_INSTANCE) private readonly db: DrizzleDb) {}

  create(userId: number, firstPrompt: string): number {
    const now = new Date();
    const title =
      firstPrompt.trim().slice(0, MAX_TITLE_LENGTH) || '新会话';
    const result = this.db
      .insert(conversations)
      .values({ userId, title, createdAt: now, updatedAt: now })
      .run();
    return Number(result.lastInsertRowid);
  }

  listByUser(userId: number): ConversationSummary[] {
    return this.db
      .select()
      .from(conversations)
      // Deleted ones are gone from the owner's history; only a super admin still
      // sees them, through the admin surface.
      .where(and(eq(conversations.userId, userId), isNull(conversations.deletedAt)))
      // Tie-broken by id: two conversations can carry the same updatedAt to the
      // millisecond, and without a second key the sidebar order is whatever the
      // scan happened to produce — which changes when the query plan does.
      .orderBy(desc(conversations.updatedAt), desc(conversations.id))
      .all()
      .map((row) => ({
        id: row.id,
        title: row.title,
        updatedAt: row.updatedAt.getTime(),
      }));
  }

  /**
   * Ownership, for the write paths: posting a turn into a conversation, and
   * taking one over. Deliberately narrower than `canRead` — a super admin may
   * read another account's conversation but never write into it — and a deleted
   * conversation is not writable either, so a stale `?c=` opens a new one.
   */
  isOwnedBy(id: number, userId: number): boolean {
    const row = this.db
      .select({ id: conversations.id })
      .from(conversations)
      .where(
        and(
          eq(conversations.id, id),
          eq(conversations.userId, userId),
          isNull(conversations.deletedAt),
        ),
      )
      .get();
    return row !== undefined;
  }

  /**
   * Hides conversations from their owner without removing anything: only
   * `deleted_at` is set, so the messages, the assets and the super admin's view
   * of them all survive. This is what the history dropdown calls.
   *
   * The hard counterpart is `deleteConversations` below, which the account
   * deletion path uses — a removed account has to take its rows and files with it.
   */
  softDeleteConversations(conversationIds: number[]): number {
    if (conversationIds.length === 0) {
      return 0;
    }
    const result = this.db
      .update(conversations)
      .set({ deletedAt: new Date() })
      .where(and(inArray(conversations.id, conversationIds), isNull(conversations.deletedAt)))
      .run();
    return result.changes;
  }

  /** Read access, for every endpoint that only returns stored content. */
  canRead(id: number, viewer: Viewer): boolean {
    const row = this.db
      .select({ id: conversations.id })
      .from(conversations)
      .innerJoin(users, eq(users.id, conversations.userId))
      .where(and(eq(conversations.id, id), readableBy(viewer)))
      .get();
    return row !== undefined;
  }

  /** Every conversation on the site, with its owner. Only for the admin surface. */
  listAll(): AdminConversationSummary[] {
    return this.db
      .select(ADMIN_SUMMARY_COLUMNS)
      .from(conversations)
      .innerJoin(users, eq(users.id, conversations.userId))
      // Tie-broken by id for the same reason as listByUser: same-millisecond
      // updates would otherwise be ordered by whatever the scan produced.
      .orderBy(desc(conversations.updatedAt), desc(conversations.id))
      .all()
      .map(toAdminSummary);
  }

  /** One conversation with its owner, for the admin read-only view. */
  summaryOf(id: number): AdminConversationSummary | null {
    const row = this.db
      .select(ADMIN_SUMMARY_COLUMNS)
      .from(conversations)
      .innerJoin(users, eq(users.id, conversations.userId))
      .where(eq(conversations.id, id))
      .get();
    return row === undefined ? null : toAdminSummary(row);
  }

  /** Owner of a conversation, for endpoints that push events rather than read. */
  ownerOf(conversationId: number): number | null {
    const row = this.db
      .select({ userId: conversations.userId })
      .from(conversations)
      .where(eq(conversations.id, conversationId))
      .get();
    return row?.userId ?? null;
  }

  views(conversationId: number): MessageView[] {
    const rows = this.db
      .select()
      .from(messages)
      .where(eq(messages.conversationId, conversationId))
      .orderBy(asc(messages.id))
      .all();

    const assetsByMessage = this.assetsByMessage(conversationId);

    return rows.map((row) => ({
      id: row.id,
      role: row.role,
      content: row.content,
      reasoning: row.reasoning,
      toolCalls: parseToolCalls(row.toolCalls),
      mode: (row.mode as Mode | null) ?? null,
      createdAt: row.createdAt.getTime(),
      assets: assetsByMessage.get(row.id) ?? [],
    }));
  }

  /**
   * `mode` is nullable because a voice turn runs in none of the four modes —
   * voice is a separate surface, not a fifth mode (docs/architecture.md §11). The
   * column and MessageView already allowed null; only the signature did not.
   */
  appendUserMessage(conversationId: number, content: string, mode: Mode | null): number {
    const result = this.db
      .insert(messages)
      .values({
        conversationId,
        role: 'user',
        content,
        reasoning: null,
        toolCalls: null,
        mode,
        createdAt: new Date(),
      })
      .run();
    this.touch(conversationId);
    return Number(result.lastInsertRowid);
  }

  /**
   * Creates the assistant row up front so assets produced mid-turn can be
   * linked to a message id (and pushed to the browser) before the turn ends.
   */
  createAssistantPlaceholder(conversationId: number, mode: Mode | null): number {
    const result = this.db
      .insert(messages)
      .values({
        conversationId,
        role: 'assistant',
        content: '',
        reasoning: null,
        toolCalls: null,
        mode,
        createdAt: new Date(),
      })
      .run();
    return Number(result.lastInsertRowid);
  }

  /**
   * A complete assistant message, used by the task worker when a background job
   * finishes: the turn that submitted it is long over, so the result lands as a
   * message of its own.
   */
  appendAssistantMessage(conversationId: number, content: string, mode: Mode | null): number {
    const result = this.db
      .insert(messages)
      .values({
        conversationId,
        role: 'assistant',
        content,
        reasoning: null,
        toolCalls: null,
        mode,
        createdAt: new Date(),
      })
      .run();
    this.touch(conversationId);
    return Number(result.lastInsertRowid);
  }

  finalizeAssistant(
    messageId: number,
    patch: { content: string; reasoning: string | null; toolCalls: ToolCallRecord[] | null },
  ): void {
    this.db
      .update(messages)
      .set({
        content: patch.content,
        reasoning: patch.reasoning,
        toolCalls: patch.toolCalls === null ? null : JSON.stringify(patch.toolCalls),
      })
      .where(eq(messages.id, messageId))
      .run();
  }

  addAsset(messageId: number, asset: StoredAsset): number {
    const result = this.db
      .insert(assets)
      .values({
        messageId,
        kind: asset.kind,
        // Stored as a name, resolved against STORAGE_DIR at read time: the row
        // must not remember a directory that only this instance knew.
        filePath: storedAssetName(asset.filePath),
        sourceUrl: asset.sourceUrl,
        mime: asset.mime,
        bytes: asset.bytes,
        createdAt: new Date(),
      })
      .run();
    return Number(result.lastInsertRowid);
  }

  touch(conversationId: number): void {
    this.db
      .update(conversations)
      .set({ updatedAt: new Date() })
      .where(eq(conversations.id, conversationId))
      .run();
  }

  /**
   * Deletes conversations with everything under them — assets, tasks, messages —
   * and then the mirrored files.
   *
   * Takes a list because it is the one implementation behind both "delete my
   * conversation" and "delete an account" (which is every conversation of that
   * account). The rows go in one transaction; the files go after it, since an
   * unlink cannot be rolled back and a leftover file is the lesser evil.
   */
  async deleteConversations(
    conversationIds: number[],
  ): Promise<{ conversations: number; assets: number }> {
    if (conversationIds.length === 0) {
      return { conversations: 0, assets: 0 };
    }

    const assetNames = this.assetNamesOf(conversationIds);

    this.db.transaction((tx) => {
      // Children before parents: assets and tasks both hang off rows that go next.
      const messageIds = tx
        .select({ id: messages.id })
        .from(messages)
        .where(inArray(messages.conversationId, conversationIds))
        .all()
        .map((row) => row.id);
      if (messageIds.length > 0) {
        tx.delete(assets).where(inArray(assets.messageId, messageIds)).run();
      }
      tx.delete(generationTasks)
        .where(inArray(generationTasks.conversationId, conversationIds))
        .run();
      tx.delete(messages).where(inArray(messages.conversationId, conversationIds)).run();
      tx.delete(conversations).where(inArray(conversations.id, conversationIds)).run();
    });

    await removeAssetFiles(assetNames);
    return { conversations: conversationIds.length, assets: assetNames.length };
  }

  /** Stored names of every asset hanging off these conversations. */
  private assetNamesOf(conversationIds: number[]): string[] {
    return this.db
      .select({ filePath: assets.filePath })
      .from(assets)
      .innerJoin(messages, eq(assets.messageId, messages.id))
      .where(inArray(messages.conversationId, conversationIds))
      .all()
      .map((row) => row.filePath);
  }

  /**
   * A named image of this conversation, used as the first frame for
   * image-to-video. Scoped to the conversation so an asset id cannot be pointed
   * at another user's image.
   */
  imageAssetIn(conversationId: number, assetId: number): { id: number; filePath: string } | null {
    const row = this.db
      .select({ id: assets.id, filePath: assets.filePath })
      .from(assets)
      .innerJoin(messages, eq(assets.messageId, messages.id))
      .where(
        and(
          eq(messages.conversationId, conversationId),
          eq(assets.kind, 'image'),
          eq(assets.id, assetId),
        ),
      )
      .get();
    return row === undefined ? null : { id: row.id, filePath: resolveAssetPath(row.filePath) };
  }

  /**
   * Unscoped by design: the signed, expiring query string is the authorization.
   * The path is resolved for the caller, which only ever wants to `sendFile` it.
   */
  assetFile(assetId: number): { filePath: string; mime: string } | null {
    const row = this.db
      .select({ filePath: assets.filePath, mime: assets.mime })
      .from(assets)
      .where(eq(assets.id, assetId))
      .get();
    return row === undefined
      ? null
      : { filePath: resolveAssetPath(row.filePath), mime: row.mime };
  }

  messageView(messageId: number): MessageView | null {
    const row = this.db
      .select()
      .from(messages)
      .where(eq(messages.id, messageId))
      .get();
    if (row === undefined) {
      return null;
    }
    const grouped = this.assetsForMessage(row.id);
    return {
      id: row.id,
      role: row.role,
      content: row.content,
      reasoning: row.reasoning,
      toolCalls: parseToolCalls(row.toolCalls),
      mode: (row.mode as Mode | null) ?? null,
      createdAt: row.createdAt.getTime(),
      assets: grouped,
    };
  }

  /**
   * Replays the conversation as text turns. Tool round-trips are not replayed:
   * their outcome is already reflected in the assistant text, and replaying
   * call ids across turns would require keeping the full provider-side item log.
   */
  buildInputItems(conversationId: number): ResponsesInputItem[] {
    const rows = this.db
      .select()
      .from(messages)
      .where(eq(messages.conversationId, conversationId))
      .orderBy(asc(messages.id))
      .all();

    const items: ResponsesInputItem[] = [];
    for (const row of rows) {
      const content = row.content.trim();
      if (content.length === 0) {
        continue;
      }
      items.push({ role: row.role, content });
    }
    return items;
  }

  /**
   * Scoped lookup for the download endpoint. An asset follows the visibility of
   * the conversation it hangs off, so the admin read-only view can show the
   * images of a conversation it may read.
   */
  findAssetForViewer(
    assetId: number,
    viewer: Viewer,
  ): (AssetView & { filePath: string }) | null {
    const row = this.db
      .select({
        id: assets.id,
        kind: assets.kind,
        mime: assets.mime,
        bytes: assets.bytes,
        filePath: assets.filePath,
      })
      .from(assets)
      .innerJoin(messages, eq(assets.messageId, messages.id))
      .innerJoin(conversations, eq(messages.conversationId, conversations.id))
      .innerJoin(users, eq(users.id, conversations.userId))
      .where(and(eq(assets.id, assetId), readableBy(viewer)))
      .get();

    return row === undefined
      ? null
      : { ...toAssetView(row), filePath: resolveAssetPath(row.filePath) };
  }

  private assetsForMessage(messageId: number): AssetView[] {
    return this.db
      .select({
        id: assets.id,
        kind: assets.kind,
        mime: assets.mime,
        bytes: assets.bytes,
      })
      .from(assets)
      .where(eq(assets.messageId, messageId))
      .orderBy(asc(assets.id))
      .all()
      .map(toAssetView);
  }

  private assetsByMessage(conversationId: number): Map<number, AssetView[]> {
    const rows = this.db
      .select({
        id: assets.id,
        messageId: assets.messageId,
        kind: assets.kind,
        mime: assets.mime,
        bytes: assets.bytes,
      })
      .from(assets)
      .innerJoin(messages, eq(assets.messageId, messages.id))
      .where(eq(messages.conversationId, conversationId))
      .orderBy(asc(assets.id))
      .all();

    const grouped = new Map<number, AssetView[]>();
    for (const row of rows) {
      const list = grouped.get(row.messageId) ?? [];
      list.push(toAssetView(row));
      grouped.set(row.messageId, list);
    }
    return grouped;
  }
}
