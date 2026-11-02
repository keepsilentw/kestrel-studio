import { Inject, Injectable } from '@nestjs/common';
import { compare } from 'bcryptjs';
import { eq } from 'drizzle-orm';
import { DRIZZLE_INSTANCE, type DrizzleDb } from '@/database/database.module';
import { users, type User } from '@/database/schema';
import { parseRole } from './roles';

export type AuthenticatedUser = Express.User;

/**
 * The single conversion from a users row to the principal carried in the
 * session, so the role cannot be forgotten on one of the two paths that build it.
 */
function toPrincipal(row: User): AuthenticatedUser {
  return { id: row.id, username: row.username, role: parseRole(row.role) };
}

@Injectable()
export class AuthService {
  constructor(@Inject(DRIZZLE_INSTANCE) private readonly db: DrizzleDb) {}

  async validate(username: string, password: string): Promise<AuthenticatedUser | null> {
    const row = this.db.select().from(users).where(eq(users.username, username)).get();
    if (row === undefined) {
      return null;
    }
    const matches = await compare(password, row.passwordHash);
    return matches ? toPrincipal(row) : null;
  }

  /** Used by the session deserializer to rebuild the principal from its id. */
  findById(id: number): AuthenticatedUser | null {
    const row = this.db.select().from(users).where(eq(users.id, id)).get();
    return row === undefined ? null : toPrincipal(row);
  }
}
