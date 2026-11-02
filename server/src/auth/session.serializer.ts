import { Injectable } from '@nestjs/common';
import { PassportSerializer } from '@nestjs/passport';
import { AuthService, type AuthenticatedUser } from './auth.service';

@Injectable()
export class SessionSerializer extends PassportSerializer {
  constructor(private readonly auth: AuthService) {
    super();
  }

  serializeUser(user: AuthenticatedUser, done: (err: Error | null, id?: number) => void): void {
    done(null, user.id);
  }

  deserializeUser(
    id: number,
    done: (err: Error | null, user?: AuthenticatedUser | null) => void,
  ): void {
    done(null, this.auth.findById(id));
  }
}
