import { ArgumentsHost, Catch, type ExceptionFilter, UnauthorizedException } from '@nestjs/common';
import type { Response } from 'express';

/**
 * Login is an HTML form post, so a failed attempt must return to the page with
 * an error flag instead of surfacing a bare 401 to the browser.
 */
@Catch(UnauthorizedException)
export class LoginRedirectFilter implements ExceptionFilter {
  catch(_exception: UnauthorizedException, host: ArgumentsHost): void {
    host.switchToHttp().getResponse<Response>().redirect('/login?error=1');
  }
}
