/**
 * Account roles.
 *
 * There is exactly one privileged level. A super admin manages accounts and may
 * read every conversation belonging to a **normal** account — never another
 * super admin's, so a privileged account's own sessions stay private to it
 * (docs/architecture.md §12).
 *
 * Both halves of that rule are stated here so a second privileged level cannot
 * be introduced by editing one guard somewhere else.
 */

export const ROLES = ['super', 'user'] as const;

export type Role = (typeof ROLES)[number];

export const ROLE_LABEL: Record<Role, string> = {
  super: '超级管理员',
  user: '普通用户',
};

/**
 * A requester, narrowed to what the visibility rule needs. `Express.User`
 * satisfies this structurally, so `req.user` can be passed as-is.
 */
export interface Viewer {
  id: number;
  role: Role;
}

/** Anything unrecognised falls back to the least privileged role. */
export function parseRole(raw: string | null): Role {
  const known = ROLES.find((role) => role === raw);
  return known ?? 'user';
}

export function isSuperAdmin(viewer: Viewer | undefined | null): boolean {
  return viewer !== undefined && viewer !== null && viewer.role === 'super';
}
