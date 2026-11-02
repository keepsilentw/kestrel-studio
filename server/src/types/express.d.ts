import type { Role } from '@/auth/roles';

/**
 * The authenticated principal attached to the request by passport.
 *
 * This is the single source of truth for the shape; application code aliases
 * it as `AuthenticatedUser` rather than redeclaring the fields.
 */
declare global {
  namespace Express {
    interface User {
      id: number;
      username: string;
      /** Account role: drives the admin surface and conversation visibility. */
      role: Role;
    }
  }
}

export {};
