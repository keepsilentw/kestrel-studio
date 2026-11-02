import {
  type CanActivate,
  type ExecutionContext,
  ForbiddenException,
  Injectable,
} from '@nestjs/common';
import type { Request, Response } from 'express';
import { isSuperAdmin } from '@/auth/roles';

/**
 * Gate for the whole /admin surface.
 *
 * Anonymous goes to the login page; a logged-in account without the privilege is
 * sent back to the chat page. POSTs get a 403 instead of that redirect — quietly
 * bouncing a rejected write to a page that looks unchanged would read as success.
 */
@Injectable()
export class SuperAdminGuard implements CanActivate {
  canActivate(context: ExecutionContext): boolean {
    const http = context.switchToHttp();
    const request = http.getRequest<Request>();

    if (request.user === undefined || request.user === null) {
      http.getResponse<Response>().redirect('/login');
      return false;
    }
    if (isSuperAdmin(request.user)) {
      return true;
    }
    if (request.method !== 'GET') {
      throw new ForbiddenException('需要超级管理员权限');
    }
    http.getResponse<Response>().redirect('/');
    return false;
  }
}
