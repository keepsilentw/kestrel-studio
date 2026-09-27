import {
  Body,
  Controller,
  Get,
  Param,
  Post,
  Req,
  Res,
  UseGuards,
} from '@nestjs/common';
import type { Request, Response } from 'express';
import { basename } from 'node:path';
import { AgentService } from '@/agent/agent.service';
import { parseMode } from '@/agent/mode';
import { AuthenticatedGuard } from '@/auth/guards';
import { isSuperAdmin } from '@/auth/roles';
import { readPositiveInt } from '@/common/http';
import { SseWriter } from '@/common/sse';
import {
  ConversationService,
  type AssetView,
  type ConversationSummary,
  type MessageView,
} from '@/conversation/conversation.service';
import { TaskEventsService } from '@/task/task-events.service';
import { TaskService } from '@/task/task.service';

interface ChatRequestBody {
  conversationId?: unknown;
  prompt?: unknown;
  mode?: unknown;
}

/**
 * The JSON/SSE surface of the chat page.
 *
 * Read endpoints gate on `canRead` and the write endpoint on `isOwnedBy`: a super
 * admin may look at any normal account's conversation, but never post into one
 * (docs/architecture.md §12).
 */
@Controller('api')
@UseGuards(AuthenticatedGuard)
export class ChatController {
  constructor(
    private readonly agent: AgentService,
    private readonly conversations: ConversationService,
    private readonly tasks: TaskService,
    private readonly events: TaskEventsService,
  ) {}

  /**
   * Streams one agent turn. The response is SSE written onto the raw response,
   * not @Sse(), because the request body carries the prompt and because the
   * client reads it with fetch + ReadableStream rather than EventSource.
   */
  @Post('chat')
  async chat(@Req() req: Request, @Res() res: Response): Promise<void> {
    const user = req.user;
    if (user === undefined) {
      res.status(401).json({ message: '未登录' });
      return;
    }

    const body = req.body as ChatRequestBody;
    const prompt = typeof body.prompt === 'string' ? body.prompt.trim() : '';
    if (prompt.length === 0) {
      res.status(400).json({ message: 'prompt 不能为空' });
      return;
    }
    const mode = parseMode(body.mode);

    const requested = readPositiveInt(body.conversationId);
    const conversationId =
      requested !== null && this.conversations.isOwnedBy(requested, user.id)
        ? requested
        : this.conversations.create(user.id, prompt);

    const writer = new SseWriter(res);
    writer.open();
    writer.send('connected', { conversationId });

    const controller = new AbortController();
    req.on('close', () => controller.abort());

    await this.agent.run({
      conversationId,
      prompt,
      mode,
      writer,
      signal: controller.signal,
    });
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

  /**
   * Long-lived stream for events that happen with no request in flight — a video
   * render finishing minutes after the turn that submitted it. Separate from the
   * turn stream, which dies with its POST.
   */
  @Get('conversations/:id/events')
  eventsStream(
    @Req() req: Request,
    @Res() res: Response,
    @Param('id') id: string,
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

    const writer = new SseWriter(res);
    writer.open();
    writer.send('connected', { conversationId });

    const subscription = this.events.subscribe(conversationId).subscribe((event) => {
      writer.send(event.type, event);
    });

    req.on('close', () => {
      subscription.unsubscribe();
      writer.close();
    });
  }

  /** Pending jobs, so a reloaded page can show that a render is still running. */
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
