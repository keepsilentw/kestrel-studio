import {
  Body,
  Controller,
  Get,
  Param,
  Post,
  Query,
  Req,
  Res,
  UseGuards,
} from '@nestjs/common';
import type { Request, Response } from 'express';
import { basename } from 'node:path';
import { parseMode } from '@/agent/mode';
import { TurnRunner } from '@/agent/turn-runner.service';
import { AuthenticatedGuard } from '@/auth/guards';
import { isSuperAdmin } from '@/auth/roles';
import { readNonNegativeInt, readPositiveInt, readString } from '@/common/http';
import { SseWriter } from '@/common/sse';
import { ConversationEventsService } from '@/conversation/conversation-events.service';
import {
  ConversationService,
  type AssetView,
  type ConversationSummary,
  type MessageView,
} from '@/conversation/conversation.service';
import { TaskService } from '@/task/task.service';

/**
 * The product API (`/api/v1`), one surface for every client — the web page
 * today, App and mini-program later (docs/roadmap.md §2.3).
 *
 * Two rules it is built around:
 *
 * - Submit and subscribe are separate calls. `POST .../turns` answers 202
 *   immediately; the turn runs in the process and its events land on the
 *   conversation journal. `GET .../stream` is the only stream, resumable via
 *   Last-Event-ID over the journal's `seq` (§2.4.1).
 * - Read endpoints gate on `canRead` and write endpoints on `isOwnedBy`: a
 *   super admin may look at any normal account's conversation, but never post
 *   into one (docs/architecture.md §12).
 *
 * Voice (`/api/voice`) stays on its own entrance: a continuous two-way socket
 * is not a turn, and its ticket endpoint is not product API (§11).
 */
@Controller('api/v1')
@UseGuards(AuthenticatedGuard)
export class ChatController {
  constructor(
    private readonly runner: TurnRunner,
    private readonly conversations: ConversationService,
    private readonly tasks: TaskService,
    private readonly events: ConversationEventsService,
  ) {}

  /** Opens a conversation. `prompt` only seeds the title from its prefix. */
  @Post('conversations')
  createConversation(
    @Req() req: Request,
    @Res() res: Response,
    @Body() body: { prompt?: unknown },
  ): void {
    const user = req.user;
    if (user === undefined) {
      res.status(401).json({ message: '未登录' });
      return;
    }
    const prompt = readString(body, 'prompt').trim();
    const conversationId = this.conversations.create(user.id, prompt);
    res.status(201).json({ conversationId });
  }

  /**
   * Submits one turn and returns as soon as its rows exist. Nothing here
   * streams: the answer arrives on `GET .../stream`, like every other event,
   * which is what lets the turn survive the client going away.
   *
   * The assistant message id is the turn's identity — `turn_started`, every
   * delta, the assets and `turn_done` all carry it.
   */
  @Post('conversations/:id/turns')
  startTurn(
    @Req() req: Request,
    @Res() res: Response,
    @Param('id') id: string,
    @Body() body: { prompt?: unknown; mode?: unknown },
  ): void {
    const user = req.user;
    if (user === undefined) {
      res.status(401).json({ message: '未登录' });
      return;
    }

    const prompt = readString(body, 'prompt').trim();
    if (prompt.length === 0) {
      res.status(400).json({ message: 'prompt 不能为空' });
      return;
    }
    const conversationId = readPositiveInt(id);
    if (conversationId === null || !this.conversations.isOwnedBy(conversationId, user.id)) {
      // 404 rather than the old silent fallback to a fresh conversation: with
      // the id in the path, inventing a different one would let a stale
      // `?c=` (deleted in another tab) write a turn the user meant elsewhere.
      // The client recreates explicitly.
      res.status(404).json({ message: 'not found' });
      return;
    }

    const messageId = this.runner.start({ conversationId, prompt, mode: parseMode(body.mode) });
    if (messageId === null) {
      // One turn per conversation: a second submit would double-bill the first.
      res.status(409).json({ message: '该会话已有轮次在进行中' });
      return;
    }
    res.status(202).json({ conversationId, messageId });
  }

  /**
   * Hides one of the caller's own conversations from their history.
   *
   * Soft on purpose: the messages, the assets and the files all stay, and a super
   * admin keeps reading the conversation — which is exactly why this is offered
   * to normal accounts only. A conversation that is not the caller's reads as
   * "not found" rather than "forbidden", so the endpoint does not confirm that it
   * exists; the role refusal is a rule about the caller, so it says so.
   */
  @Post('conversations/:id/delete')
  deleteConversation(
    @Req() req: Request,
    @Res() res: Response,
    @Param('id') id: string,
  ): void {
    const user = req.user;
    const conversationId = readPositiveInt(id);
    if (user === undefined || conversationId === null) {
      res.status(404).json({ message: 'not found' });
      return;
    }
    if (isSuperAdmin(user)) {
      res.status(403).json({ message: '超级管理员不提供会话删除' });
      return;
    }
    if (!this.conversations.isOwnedBy(conversationId, user.id)) {
      res.status(404).json({ message: 'not found' });
      return;
    }

    this.conversations.softDeleteConversations([conversationId]);
    res.status(200).json({ ok: true });
  }

  @Get('conversations')
  list(@Req() req: Request): ConversationSummary[] {
    const user = req.user;
    return user === undefined ? [] : this.conversations.listByUser(user.id);
  }

  @Get('conversations/:id/messages')
  async messages(@Req() req: Request, @Param('id') id: string): Promise<MessageView[]> {
    const user = req.user;
    const conversationId = readPositiveInt(id);
    if (user === undefined || conversationId === null) {
      return [];
    }
    if (!this.conversations.canRead(conversationId, user)) {
      return [];
    }
    return this.conversations.views(conversationId);
  }

  @Get('conversations/:id/tasks')
  activeTasks(@Req() req: Request, @Param('id') id: string): { id: number; kind: string; status: string }[] {
    const user = req.user;
    const conversationId = readPositiveInt(id);
    if (user === undefined || conversationId === null) {
      return [];
    }
    if (!this.conversations.canRead(conversationId, user)) {
      return [];
    }
    return this.tasks.activeByConversation(conversationId).map((task) => ({
      id: task.id,
      kind: task.kind,
      status: task.status,
    }));
  }

  /**
   * The conversation-level stream: journaled events, resumable.
   *
   * Every frame that carries an event also carries `id: <seq>` (the journal
   * row id), so an EventSource reconnect sends the cursor back as
   * Last-Event-ID and the missed events replay. A first connection knows
   * where it stands through `?from=`: the seq the page was painted at, or 0
   * for a conversation it just created and wants from the start.
   *
   * With neither cursor the stream catches up: it replays a still-running
   * turn from its first event and otherwise starts live — SSR already drew
   * the finished history.
   *
   * A cursor older than the retained window yields one `resync` frame instead
   * of a partial replay that would look complete; the client rebuilds from
   * the persisted rows. Setup is synchronous end to end (better-sqlite3, and
   * publish inserts before fanning out), so no event can slip between the
   * replay query and the subscription.
   */
  @Get('conversations/:id/stream')
  stream(
    @Req() req: Request,
    @Res() res: Response,
    @Param('id') id: string,
    @Query('from') from: string | undefined,
  ): void {
    const user = req.user;
    const conversationId = readPositiveInt(id);
    if (user === undefined || conversationId === null) {
      res.status(401).json({ message: '未登录' });
      return;
    }
    if (!this.conversations.canRead(conversationId, user)) {
      res.status(404).json({ message: 'not found' });
      return;
    }

    const requested =
      readNonNegativeInt(req.headers['last-event-id']) ?? readNonNegativeInt(from);
    const cursor = requested ?? this.runner.catchUpCursor(conversationId);

    const writer = new SseWriter(res);
    writer.open();

    if (requested !== null && this.events.hasGap(conversationId, requested)) {
      writer.send('resync', { conversationId, cursor: requested });
    }
    writer.send('connected', { conversationId });

    let lastWritten = cursor;
    for (const published of this.events.replayAfter(conversationId, cursor)) {
      writer.send(published.event.type, published.event, published.seq);
      lastWritten = published.seq;
    }

    const subscription = this.events
      .subscribe(conversationId)
      .subscribe((published) => {
        // The replay above already covered this seq when the event predates
        // the snapshot; guard the boundary rather than trust the ordering.
        if (published.seq > lastWritten) {
          writer.send(published.event.type, published.event, published.seq);
          lastWritten = published.seq;
        }
      });

    req.on('close', () => {
      subscription.unsubscribe();
      writer.close();
    });
  }

  /** Image generation URLs are provider-side and expire; serve from disk instead. */
  @Get('assets/:id/download')
  download(@Req() req: Request, @Param('id') id: string, @Res() res: Response): void {
    const user = req.user;
    const assetId = readPositiveInt(id);
    if (user === undefined || assetId === null) {
      res.status(404).json({ message: 'not found' });
      return;
    }

    const asset: AssetView & { filePath: string } | null = this.conversations.findAssetForViewer(
      assetId,
      user,
    );
    if (asset === null) {
      res.status(404).json({ message: 'not found' });
      return;
    }

    res.setHeader('Content-Type', asset.mime);
    res.setHeader('Content-Disposition', `inline; filename="${basename(asset.filePath)}"`);
    res.sendFile(asset.filePath, (error?: Error) => {
      if (error !== undefined && !res.headersSent) {
        res.status(404).json({ message: 'not found' });
      }
    });
  }
}
