import { Controller, Post, Req, Res, UseFilters, UseGuards } from '@nestjs/common';
import type { Request, Response } from 'express';
import { LocalAuthGuard } from './guards';
import { LoginRedirectFilter } from './login-redirect.filter';

@Controller()
export class AuthController {
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
}
