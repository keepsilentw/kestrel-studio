import { Controller, Get, Query, Req, Res, UseGuards } from '@nestjs/common';
import type { Request, Response } from 'express';
import { TurnRunner } from '@/agent/turn-runner.service';
import { ViewAuthGuard } from '@/auth/guards';
import { isSuperAdmin } from '@/auth/roles';
import { readPositiveInt } from '@/common/http';
import { ConversationService } from '@/conversation/conversation.service';
import type { ConversationSummary } from '@/conversation/conversation.service';
import { TaskService } from '@/task/task.service';
import {
  buildTurnAnchors,
  renderMessages,
  type TurnAnchor,
} from './render';

interface RenderedConversation extends ConversationSummary {
  isActive: boolean;
}

interface PendingTask {
  id: number;
  kind: string;
  statusLabel: string;
}

const TASK_STATUS_LABEL: Record<string, string> = {
  queued: '排队中',
  running: '渲染中',
};

/** The sidebar's first paint; the client appends older pages on demand. */
const HISTORY_PAGE_SIZE = 50;
/** The main column's first paint and its growth step. */
const MESSAGE_PAGE_DEFAULT = 50;
const MESSAGE_PAGE_MAX = 1000;

@Controller()
export class ViewController {
  constructor(
    private readonly conversations: ConversationService,
    private readonly tasks: TaskService,
    private readonly runner: TurnRunner,
  ) {}

  @UseGuards(ViewAuthGuard)
  @Get('voice')
  voice(@Req() req: Request, @Res() res: Response): void {
    const user = req.user;
    if (user === undefined) {
      res.redirect('/login');
      return;
    }
    // No conversation is rendered: a call creates its own on the server when the
    // socket opens, and the client appends everything from there.
    res.render('voice', { user });
  }

  /**
   * The account's own device tokens. The frameworkless client script fills the
   * list and drives the same JSON endpoints native hosts use, so the token UI is
   * not duplicated in either browser bundle.
   */
  @UseGuards(ViewAuthGuard)
  @Get('settings/tokens')
  tokens(@Req() req: Request, @Res() res: Response): void {
    const user = req.user;
    if (user === undefined) {
      res.redirect('/login');
      return;
    }
    res.render('tokens', { user });
  }

  @Get('login')
  login(@Query('error') error: string | undefined, @Res() res: Response): void {
    const user = res.req.user;
    if (user !== undefined && user !== null) {
      res.redirect('/');
      return;
    }
    res.render('login', { error: error === '1' });
  }

  /**
   * Renders the first paint: shell, conversation list and the active
   * conversation. Everything after that is appended by the client.
   */
  @UseGuards(ViewAuthGuard)
  @Get()
  chat(
    @Req() req: Request,
    @Res() res: Response,
    @Query('c') c: string | undefined,
    @Query('new') fresh: string | undefined,
    @Query('messages') messagesRaw: string | undefined,
  ): void {
    const user = req.user;
    if (user === undefined) {
      res.redirect('/login');
      return;
    }

    let list = this.conversations.listByUser(user.id, { limit: HISTORY_PAGE_SIZE });
    const requested = readPositiveInt(c);
    // `?new=1` names the empty shell explicitly. The fallback below is not
    // query-driven, so an absent `?c=` cannot mean "start fresh" — see
    // docs/implementation-notes.md §23 for the reload that used to resurrect it.
    const active =
      fresh === '1'
        ? null
        : requested !== null && this.conversations.isOwnedBy(requested, user.id)
          ? requested
          : (list[0]?.id ?? null);

    // An older `?c=` conversation may sit past the first page; keep its row so the
    // sidebar highlights what the main column shows.
    if (active !== null && !list.some((item) => item.id === active)) {
      const summary = this.conversations.summaryByOwner(active, user.id);
      if (summary !== null) {
        list = [summary, ...list];
      }
    }

    const conversations: RenderedConversation[] = list.map((item) => ({
      ...item,
      isActive: item.id === active,
    }));

    const pendingTasks: PendingTask[] =
      active === null
        ? []
        : this.tasks.activeByConversation(active).map((task) => ({
            id: task.id,
            kind: task.kind,
            statusLabel: TASK_STATUS_LABEL[task.status] ?? task.status,
          }));

    const messageLimit = Math.min(
      Math.max(
        readPositiveInt(messagesRaw) ?? MESSAGE_PAGE_DEFAULT,
        MESSAGE_PAGE_DEFAULT,
      ),
      MESSAGE_PAGE_MAX,
    );
    const page =
      active === null
        ? { views: [], hasMore: false }
        : this.conversations.viewsPage(active, { limit: messageLimit });
    // An assistant placeholder of a still-running turn is not painted: the
    // stream replays that turn from its first event (startSeq is set to
    // before it), and a half-drawn row would only double or contradict it.
    const views = page.views.filter(
      (view) => !(view.role === 'assistant' && view.content.length === 0),
    );
    const turnAnchors: TurnAnchor[] = buildTurnAnchors(views);
    const startSeq = active === null ? null : this.runner.catchUpCursor(active);
    const nextLimit = Math.min(messageLimit + MESSAGE_PAGE_DEFAULT, MESSAGE_PAGE_MAX);
    const olderHref =
      active !== null && page.hasMore && nextLimit > messageLimit
        ? `/?c=${active}&messages=${nextLimit}`
        : null;

    res.render('chat', {
      user,
      conversations,
      activeConversationId: active,
      startSeq,
      messages: renderMessages(views),
      olderHref,
      pendingTasks,
      turnAnchors,
      // Deleting from the history list is a normal account's affordance: a super
      // admin sees everyone's history and does not hide its own from them.
      canDeleteConversations: !isSuperAdmin(user),
    });
  }
}
