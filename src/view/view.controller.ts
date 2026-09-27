import { Controller, Get, Query, Req, Res, UseGuards } from '@nestjs/common';
import type { Request, Response } from 'express';
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

@Controller()
export class ViewController {
  constructor(
    private readonly conversations: ConversationService,
    private readonly tasks: TaskService,
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
  chat(@Req() req: Request, @Res() res: Response, @Query('c') c: string | undefined): void {
    const user = req.user;
    if (user === undefined) {
      res.redirect('/login');
      return;
    }

    const list = this.conversations.listByUser(user.id);
    const requested = readPositiveInt(c);
    const active =
      requested !== null && this.conversations.isOwnedBy(requested, user.id)
        ? requested
        : (list[0]?.id ?? null);

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

    const views = active === null ? [] : this.conversations.views(active);
    const turnAnchors: TurnAnchor[] = buildTurnAnchors(views);

    res.render('chat', {
      user,
      conversations,
      activeConversationId: active,
      messages: renderMessages(views),
      pendingTasks,
      turnAnchors,
      // Deleting from the history list is a normal account's affordance: a super
      // admin sees everyone's history and does not hide its own from them.
      canDeleteConversations: !isSuperAdmin(user),
    });
  }
}
