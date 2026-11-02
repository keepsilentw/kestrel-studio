import { Injectable, UnauthorizedException } from '@nestjs/common';
import { PassportStrategy } from '@nestjs/passport';
import { Strategy } from 'passport-local';
import { AuthService, type AuthenticatedUser } from './auth.service';

@Injectable()
export class LocalStrategy extends PassportStrategy(Strategy) {
  constructor(private readonly auth: AuthService) {
    super({ usernameField: 'username', passwordField: 'password' });
  }

  async validate(username: string, password: string): Promise<AuthenticatedUser> {
    const user = await this.auth.validate(username, password);
    if (user === null) {
      throw new UnauthorizedException('用户名或密码错误');
    }
    return user;
  }
}
