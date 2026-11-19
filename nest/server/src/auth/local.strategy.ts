import {
  HttpException,
  HttpStatus,
  Injectable,
  UnauthorizedException,
} from '@nestjs/common';
import { PassportStrategy } from '@nestjs/passport';
import type { Request } from 'express';
import { Strategy } from 'passport-local';
import { clientIp } from '@/common/client-ip';
import { AuthService, type AuthenticatedUser } from './auth.service';
import { LoginLimiter } from './login-limiter.service';

@Injectable()
export class LocalStrategy extends PassportStrategy(Strategy) {
  constructor(
    private readonly auth: AuthService,
    private readonly limiter: LoginLimiter,
  ) {
    super({
      usernameField: 'username',
      passwordField: 'password',
      passReqToCallback: true,
    });
  }

  async validate(
    request: Request,
    username: string,
    password: string,
  ): Promise<AuthenticatedUser> {
    const ip = clientIp(request);
    const wait = this.limiter.blockedFor(ip, username);
    if (wait !== null) {
      throw new HttpException(
        `登录尝试过于频繁，请 ${wait} 秒后再试`,
        HttpStatus.TOO_MANY_REQUESTS,
      );
    }

    const user = await this.auth.validate(username, password);
    if (user === null) {
      this.limiter.recordFailure(ip, username);
      throw new UnauthorizedException('用户名或密码错误');
    }
    this.limiter.recordSuccess(ip, username);
    return user;
  }
}
