import type { RequestHandler } from 'express';
import type { AuthService } from './auth.service';
import type { TokenService } from './token.service';

function readBearer(header: string): string | null {
  const space = header.indexOf(' ');
  if (space < 0) {
    return null;
  }
  if (header.slice(0, space).toLowerCase() !== 'bearer') {
    return null;
  }
  const token = header.slice(space + 1).trim();
  return token.length === 0 ? null : token;
}

/**
 * Resolves `Authorization: Bearer` before the guards run, after `passport.session()`
 * has already attached a cookie principal. Bearer wins with no identity merge: an
 * invalid Bearer clears `request.user` instead of letting the cookie through
 * (docs/roadmap.md §5.1).
 */
export function createBearerMiddleware(
  tokens: TokenService,
  auth: AuthService,
): RequestHandler {
  return (request, _response, next) => {
    const header = request.headers.authorization;
    if (header === undefined) {
      next();
      return;
    }
    try {
      const token = readBearer(header);
      const userId = token === null ? null : tokens.resolveBearer(token);
      const user = userId === null ? null : auth.findById(userId);
      if (user === null) {
        delete request.user;
      } else {
        request.user = user;
      }
      next();
    } catch (error: unknown) {
      next(error);
    }
  };
}
