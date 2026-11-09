import { Inject, Injectable, Logger, type OnApplicationBootstrap } from '@nestjs/common';
import { and, asc, count, desc, eq, inArray, isNull, lt, or, type SQL } from 'drizzle-orm';
import { closeSync, openSync, readSync, statSync } from 'node:fs';
import type { ResponsesInputItem } from '@/bailian/responses-client';
import type { Mode } from '@/agent/mode';
import { isSuperAdmin, type Viewer } from '@/auth/roles';
import { WRITER_ID } from '@/common/writer';
import { DRIZZLE_INSTANCE, type DrizzleDb } from '@/database/database.module';
import { assets, conversations, conversationEvents, generationTasks, messages, users } from '@/database/schema';
import { removeAssetFiles, resolveAssetPath, storedAssetName } from '@/media/asset-path';
import { imageDims } from '@/media/image-dims';
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
  /**
   * Pixel dimensions when the server read them off the stored bytes. Same three
   * senses of null as `rust/server`'s `AssetView`: a video, an unreadable header, or a
   * row predating the columns. Never a guess, and the template leaves the attributes
   * off unless **both** are present.
   */
  width: number | null;
  height: number | null;
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
 * How old an assistant placeholder with no `owner` has to be before this stack's boot
 * recovery may end it. Rows written by a build that predates the column are
 * indistinguishable from rows the *other* stack is writing right now, and a turn — one
 * image or video submit plus one streamed reply — is well under a minute, so five is a
 * wide margin. `rust/server/src/conversation/repo.rs` uses the same number: the two
 * stacks share one table, so the grace period is a protocol, not a detail.
 */
const UNOWNED_STALE_AFTER_MS = 5 * 60 * 1000;

/**
 * How many image rows one boot tries to fill with dimensions. Bounded so a large
 * legacy library cannot stretch startup; the rest is picked up by the next boot.
 * Same number as `rust/server/src/conversation/repo.rs`'s `DIMENSION_BACKFILL_LIMIT`.
 */
const DIMENSION_BACKFILL_LIMIT = 500;

/**
 * Bytes read from the head of each file. PNG, GIF and WebP keep their dimensions in
 * the first 30 bytes; JPEG can hide its start-of-frame behind an EXIF thumbnail.
 */
const DIMENSION_READ_BYTES = 256 * 1024;

/**
 * The first `DIMENSION_READ_BYTES` of a file, or null when it cannot be read. A
 * deleted asset is not an error here — just a row that stays without dimensions.
 */
function readHead(path: string): Buffer | null {
  let fd: number | null = null;
  try {
    const size = Math.min(statSync(path).size, DIMENSION_READ_BYTES);
    const buffer = Buffer.alloc(size);
    fd = openSync(path, 'r');
    const read = readSync(fd, buffer, 0, size, 0);
    return read > 0 ? buffer.subarray(0, read) : null;
  } catch {
    return null;
  } finally {
    if (fd !== null) {
      closeSync(fd);
    }
  }
}

/**
 * Where the browser fetches an asset from. Single source of the format: it is
 * also what the turn stream and the voice socket hand to the client.
 */
export function assetUrl(assetId: number): string {
  return `/api/v1/assets/${assetId}/download`;
}

function toAssetView(row: {
  id: number;
  kind: 'image' | 'video';
  mime: string;
  bytes: number;
  width?: number | null;
  height?: number | null;
}): AssetView {
  return {
    id: row.id,
    kind: row.kind,
    url: assetUrl(row.id),
    mime: row.mime,
    bytes: row.bytes,
    // Optional on the input: a query that has not selected the columns (the download
    // path, which renders no image) must still produce a well-formed view.
    width: row.width ?? null,
    height: row.height ?? null,
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
// The interface is not decoration: Nest finds the hook by name at runtime, so without
// `implements OnApplicationBootstrap` a typo in the method would silently never run.
export class ConversationService implements OnApplicationBootstrap {
  constructor(@Inject(DRIZZLE_INSTANCE) private readonly db: DrizzleDb) {}

  private readonly logger = new Logger(ConversationService.name);

  /**
   * Fills `assets.width` / `assets.height` for rows written before those columns.
   *
   * Without this the dimensions fix applies only to new images and every existing
   * conversation keeps reflowing — which is the whole symptom. Mirrors
   * `rust/server/src/conversation/repo.rs::backfill_asset_dimensions`, including the
   * reason it runs at boot: the database file survives deploys, so data migrations
   * live in the process here rather than in a migration tool.
   */
  onApplicationBootstrap(): void {
    try {
      const { filled, unreadable } = this.backfillAssetDimensions();
      if (filled === 0 && unreadable === 0) {
        return;
      }
      this.logger.log(
        `backfilled image dimensions: filled=${filled} unreadable=${unreadable} remaining=${this.assetsWithoutDimensions()}`,
      );
    } catch (error: unknown) {
      // A skipped backfill costs a layout jump. Refusing to boot costs the service.
      this.logger.warn(
        `asset dimension backfill did not run: ${error instanceof Error ? error.message : String(error)}`,
      );
    }
  }

  /**
   * Idempotent and safe to run twice at once: it selects only rows where the width is
   * NULL and its UPDATE repeats that condition, so two backends racing over one file
   * write the same numbers. A missing or unreadable file stays NULL — no invented
   * pair — and the next boot tries again, bounded by the limit above.
   */
  backfillAssetDimensions(limit = DIMENSION_BACKFILL_LIMIT): { filled: number; unreadable: number } {
    const rows = this.db
      .select({ id: assets.id, filePath: assets.filePath })
      .from(assets)
      .where(and(eq(assets.kind, 'image'), isNull(assets.width)))
      .orderBy(asc(assets.id))
      .limit(limit)
      .all();
    if (rows.length === 0) {
      return { filled: 0, unreadable: 0 };
    }

    let filled = 0;
    let unreadable = 0;
    for (const row of rows) {
      // resolveAssetPath is the only way to read these: the same column holds bare
      // names and the absolute paths written before the storage-directory change.
      const bytes = readHead(resolveAssetPath(row.filePath));
      const dimensions = bytes === null ? null : imageDims(bytes);
      if (dimensions === null) {
        unreadable += 1;
        continue;
      }
      this.db
        .update(assets)
        .set({ width: dimensions[0], height: dimensions[1] })
        .where(and(eq(assets.id, row.id), isNull(assets.width)))
        .run();
      filled += 1;
    }
    return { filled, unreadable };
  }

  /** How many image rows are still waiting for dimensions, after one bounded pass. */
  assetsWithoutDimensions(): number {
    const row = this.db
      .select({ n: count() })
      .from(assets)
      .where(and(eq(assets.kind, 'image'), isNull(assets.width)))
      .get();
    return row?.n ?? 0;
  }

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
        owner: WRITER_ID,
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
        owner: WRITER_ID,
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
        // Every row this stack writes carries its identity, not just the placeholder:
        // the column means "who wrote this row", so the recovery predicate never has to
        // guess which rows a turn is still behind.
        owner: WRITER_ID,
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

  /**
   * The assistant placeholder of a turn still running in this process: the row
   * exists before the text does. The stream's catch-up starting point keys off
   * it — a fresh connection to a busy conversation replays the live turn from
   * its first event instead of showing nothing until it finalizes.
   */
  unfinishedAssistant(conversationId: number): number | null {
    const row = this.db
      .select({ id: messages.id })
      .from(messages)
      .where(
        and(
          eq(messages.conversationId, conversationId),
          eq(messages.role, 'assistant'),
          eq(messages.content, ''),
        ),
      )
      .orderBy(desc(messages.id))
      .get();
    return row?.id ?? null;
  }

  /**
   * Ends assistant rows a previous process left unfinished, at startup.
   *
   * A hard kill (restart, crash) stops a running turn with its placeholder
   * empty. The empty content itself is not the danger — `buildInputItems`
   * skipping it would orphan the user message, and two consecutive user
   * messages make the model re-run the failed request as if it were new
   * (that once duplicated a paid video submission; CLAUDE.md §Agent). A
   * graceful stop finalizes through AgentService's `finally`; this covers
   * everything that path never got to run.
   *
   * It only ends rows **this stack wrote**, because the two backends share this
   * database file and neither can see the other's live turns (docs/architecture.md
   * §13.2, measured 2026-11-07 on a copy of the live database). The rule per owner:
   * own rows always — a new process of the same stack is exactly who is meant to clean
   * up after the old one — foreign rows never, and a row with no owner at all only once
   * it is older than `UNOWNED_STALE_AFTER_MS`, so a fresh one belonging to the other
   * side is not grabbed mid-flight.
   *
   * Returns one `{ conversationId, messageId }` per row it closed, because the
   * page that was watching the turn has no way to learn it ended — its composer
   * stays locked on "正在生成…" until someone reloads (docs/verification.md §24.4).
   * `TurnRunnerService` journals a `turn_error` for each, which is what a
   * reconnecting client releases on. Same contract as `rust/server`'s
   * `abandon_stale_placeholders`.
   */
  abandonStalePlaceholders(): Array<{ conversationId: number; messageId: number }> {
    const mine = or(
      eq(messages.owner, WRITER_ID),
      // An unowned row is either this table's own past or the other stack running a
      // build that predates the column, and the two are indistinguishable from the
      // row alone — hence the grace period rather than an unconditional grab.
      and(
        isNull(messages.owner),
        lt(messages.createdAt, new Date(Date.now() - UNOWNED_STALE_AFTER_MS)),
      ),
    );
    const unfinished = and(
      eq(messages.role, 'assistant'),
      eq(messages.content, ''),
      mine,
    );
    const stale = this.db
      .select({ conversationId: messages.conversationId, id: messages.id })
      .from(messages)
      .where(unfinished)
      .all();
    if (stale.length === 0) {
      return [];
    }
    this.db.update(messages).set({ content: '本轮处理失败：服务在轮次完成前中断。' }).where(unfinished).run();
    return stale.map((row) => ({ conversationId: row.conversationId, messageId: row.id }));
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
        // Whatever the header probe found. Null is a real answer, not a missing one.
        width: asset.width,
        height: asset.height,
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
      tx.delete(conversationEvents)
        .where(inArray(conversationEvents.conversationId, conversationIds))
        .run();
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
        width: assets.width,
        height: assets.height,
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
        width: assets.width,
        height: assets.height,
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
