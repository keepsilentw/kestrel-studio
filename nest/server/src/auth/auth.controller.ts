import {
  BadRequestException,
  Body,
  Controller,
  HttpCode,
  Post,
  Req,
  Res,
  UnauthorizedException,
  UseFilters,
  UseGuards,
} from '@nestjs/common';
import type { Request, Response } from 'express';
import { readString } from '@/common/http';
import { AuthService } from './auth.service';
import { LocalAuthGuard } from './guards';
import { LoginRedirectFilter } from './login-redirect.filter';

@Controller()
export class AuthController {
  constructor(private readonly auth: AuthService) {}

  /**
   * The credential check is done by LocalAuthGuard, but @nestjs/passport's
   * AuthGuard only authenticates: it attaches the principal to request.user and
   * returns. It never calls req.logIn, so without the explicit call below the
   * session is never written and the next request is anonymous again.
   */
  @UseGuards(LocalAuthGuard)
  @UseFilters(LoginRedirectFilter)
  @Post('login')
  login(@Req() req: Request, @Res() res: Response): void {
    const user = req.user;
    if (user === undefined) {
      res.redirect('/login?error=1');
      return;
    }

    req.logIn(user, (error?: Error) => {
      if (error !== undefined) {
        res.redirect('/login?error=1');
        return;
      }
      res.redirect('/');
    });
  }

  @Post('logout')
  logout(@Req() req: Request, @Res() res: Response): void {
    req.logout((): void => {
      req.session.destroy((): void => {
        res.redirect('/login');
      });
    });
  }

  /**
   * The JSON twin of the HTML form, for hosts that have no cookie jar. It returns
   * the same session sid the cookie carries, once; Bearer resolution accepts that
   * sid directly, so the host can use it immediately and later exchange it for a
   * revocable device token through `POST /api/v1/tokens`.
   */
  @Post('api/v1/session')
  @HttpCode(200)
  async session(
    @Body() body: unknown,
    @Req() req: Request,
  ): Promise<{ token: string; userId: number }> {
    const username = readString(body, 'username');
    const password = readString(body, 'password');
    if (username.length === 0 || password.length === 0) {
      throw new BadRequestException('用户名和密码不能为空');
    }
    const user = await this.auth.validate(username, password);
    if (user === null) {
      throw new UnauthorizedException('用户名或密码错误');
    }
    await new Promise<void>((resolve, reject) => {
      req.logIn(user, (error?: Error) => {
        if (error === undefined) {
          resolve();
        } else {
          reject(error);
        }
      });
    });
    return { token: req.sessionID, userId: user.id };
  }
}
