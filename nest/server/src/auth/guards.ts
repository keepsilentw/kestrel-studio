import {
  type CanActivate,
  type ExecutionContext,
  Injectable,
  UnauthorizedException,
} from '@nestjs/common';
import { AuthGuard } from '@nestjs/passport';
import type { Request, Response } from 'express';

export class LocalAuthGuard extends AuthGuard('local') {}

/** For JSON endpoints: a missing session becomes 401. */
@Injectable()
export class AuthenticatedGuard implements CanActivate {
  canActivate(context: ExecutionContext): boolean {
    const request = context.switchToHttp().getRequest<Request>();
    if (request.user !== undefined && request.user !== null) {
      return true;
    }
    throw new UnauthorizedException('未登录');
  }
}

/** For HTML routes: a missing session becomes a redirect to the login page. */
@Injectable()
export class ViewAuthGuard implements CanActivate {
  canActivate(context: ExecutionContext): boolean {
    const http = context.switchToHttp();
    const request = http.getRequest<Request>();
    if (request.user !== undefined && request.user !== null) {
      return true;
    }
    http.getResponse<Response>().redirect('/login');
    return false;
  }
}
