import { Body, Controller, Get, Param, Post, Query, Req, Res, UseGuards } from '@nestjs/common';
import type { Request, Response } from 'express';
import { SuperAdminGuard } from '@/admin/admin.guard';
import { AdminService, type AccountView } from '@/admin/admin.service';
import { asRecord, readOptionalString } from '@/common/args';
import { readPositiveInt } from '@/common/http';
import { ConversationService } from '@/conversation/conversation.service';
import { formatTimestamp, renderMessages } from '@/view/render';

/**
 * The super admin's surface: account management and a read-only view of every
 * normal account's conversations. Server-rendered forms with POST + redirect,
 * exactly like /login and /logout — there is no client script on these pages.
 */

interface Notice {
  text: string;
  isError: boolean;
}

interface PresentedAccount extends AccountView {
  createdAtLabel: string;
}

interface AccountChip {
  id: number;
  username: string;
  isActive: boolean;
}

const NOTICE_BY_CODE = new Map<string, Notice>([
  ['created', { text: '账号已创建', isError: false }],
  ['duplicate', { text: '该用户名已存在', isError: true }],
  ['invalid', { text: '用户名需 3-32 位字母、数字、下划线或短横线；密码至少 6 位', isError: true }],
  ['password-reset', { text: '密码已重置', isError: false }],
  ['removed', { text: '账号及其全部会话与资产已删除', isError: false }],
  ['self', { text: '不能删除当前登录的账号', isError: true }],
  ['last-super', { text: '不能删除最后一个超级管理员', isError: true }],
  ['missing', { text: '账号或会话不存在', isError: true }],
]);

function readNotice(code: string | undefined): Notice | null {
  return code === undefined ? null : (NOTICE_BY_CODE.get(code) ?? null);
}

function presentAccount(account: AccountView): PresentedAccount {
  return { ...account, createdAtLabel: formatTimestamp(account.createdAt) };
}

/** Passwords are hashed as typed: trimming one would silently change it. */
function readRawField(body: Record<string, unknown>, key: string): string {
  const value = body[key];
  return typeof value === 'string' ? value : '';
}

function noticeForWriteFailure(reason: 'username' | 'password' | 'duplicate'): string {
  return reason === 'duplicate' ? 'duplicate' : 'invalid';
}

@Controller('admin')
@UseGuards(SuperAdminGuard)
export class AdminController {
  constructor(
    private readonly admin: AdminService,
    private readonly conversations: ConversationService,
  ) {}

  @Get()
  index(
    @Req() req: Request,
    @Res() res: Response,
    @Query('notice') notice: string | undefined,
  ): void {
    res.render('admin', {
      user: req.user,
      accounts: this.admin.list().map(presentAccount),
      notice: readNotice(notice),
    });
  }

  /** Site-wide conversation list; `?user=` narrows it to one account. */
  @Get('conversations')
  conversationsPage(
    @Req() req: Request,
    @Res() res: Response,
    @Query('user') user: string | undefined,
    @Query('notice') notice: string | undefined,
  ): void {
    const accounts = this.admin.list();
    const filter = readPositiveInt(user);
    const owner = filter === null ? null : (accounts.find((item) => item.id === filter) ?? null);

    const all = this.conversations.listAll();
    const rows = all
      .filter((item) => owner === null || item.ownerId === owner.id)
      .map((item) => ({
        ...item,
        updatedAtLabel: formatTimestamp(item.updatedAt),
        // Shown rather than filtered out: a soft-deleted conversation is one its
        // owner hid, and reading it is the super admin's reason for having this page.
        deletedAtLabel: item.deletedAt === null ? null : formatTimestamp(item.deletedAt),
      }));
    const chips: AccountChip[] = accounts.map((item) => ({
      id: item.id,
      username: item.username,
      isActive: owner !== null && owner.id === item.id,
    }));

    res.render('admin-conversations', {
      user: req.user,
      conversations: rows,
      chips,
      ownerName: owner?.username ?? null,
      siteTotal: all.length,
      notice: readNotice(notice),
    });
  }

  /** Read-only replay of one conversation, whoever it belongs to. */
  @Get('conversations/:id')
  conversation(@Req() req: Request, @Res() res: Response, @Param('id') id: string): void {
    const user = req.user;
    const conversationId = readPositiveInt(id);
    if (user === undefined || conversationId === null) {
      res.redirect('/admin/conversations?notice=missing');
      return;
    }
    if (!this.conversations.canRead(conversationId, user)) {
      res.redirect('/admin/conversations?notice=missing');
      return;
    }

    const summary = this.conversations.summaryOf(conversationId);
    if (summary === null) {
      res.redirect('/admin/conversations?notice=missing');
      return;
    }

    res.render('admin-conversation', {
      user,
      conversation: {
        ...summary,
        updatedAtLabel: formatTimestamp(summary.updatedAt),
        deletedAtLabel: summary.deletedAt === null ? null : formatTimestamp(summary.deletedAt),
      },
      messages: renderMessages(this.conversations.views(conversationId)),
      isOwn: summary.ownerId === user.id,
    });
  }

  /** One account: its facts, the password reset and the way to delete it. */
  @Get('users/:id')
  account(
    @Req() req: Request,
    @Res() res: Response,
    @Param('id') id: string,
    @Query('notice') notice: string | undefined,
  ): void {
    const accountId = readPositiveInt(id);
    const account = accountId === null ? null : this.admin.find(accountId);
    if (account === null) {
      res.redirect('/admin?notice=missing');
      return;
    }
    res.render('admin-user', {
      user: req.user,
      account: presentAccount(account),
      notice: readNotice(notice),
    });
  }

  @Get('users/:id/delete')
  confirmDelete(@Req() req: Request, @Res() res: Response, @Param('id') id: string): void {
    const accountId = readPositiveInt(id);
    const account = accountId === null ? null : this.admin.find(accountId);
    if (account === null) {
      res.redirect('/admin?notice=missing');
      return;
    }
    res.render('admin-delete', { user: req.user, account: presentAccount(account) });
  }

  @Post('users')
  async createUser(@Body() body: unknown, @Res() res: Response): Promise<void> {
    const fields = asRecord(body);
    const result = await this.admin.create(
      readOptionalString(fields, 'username') ?? '',
      readRawField(fields, 'password'),
    );
    res.redirect(`/admin?notice=${result.ok ? 'created' : noticeForWriteFailure(result.reason)}`);
  }

  @Post('users/:id/password')
  async resetPassword(
    @Body() body: unknown,
    @Param('id') id: string,
    @Res() res: Response,
  ): Promise<void> {
    const accountId = readPositiveInt(id);
    if (accountId === null) {
      res.redirect('/admin?notice=missing');
      return;
    }
    const result = await this.admin.resetPassword(
      accountId,
      readRawField(asRecord(body), 'password'),
    );
    if (!result.ok && result.reason === 'missing') {
      res.redirect('/admin?notice=missing');
      return;
    }
    // Back to the account page: a rejected password reads as 'invalid', the same
    // notice the create form shows for it.
    res.redirect(`/admin/users/${accountId}?notice=${result.ok ? 'password-reset' : 'invalid'}`);
  }

  @Post('users/:id/delete')
  async removeUser(
    @Param('id') id: string,
    @Req() req: Request,
    @Res() res: Response,
  ): Promise<void> {
    const user = req.user;
    const accountId = readPositiveInt(id);
    if (user === undefined || accountId === null) {
      res.redirect('/admin?notice=missing');
      return;
    }
    const result = await this.admin.remove(accountId, user);
    res.redirect(`/admin?notice=${result.ok ? 'removed' : result.reason}`);
  }
}
