import { describe, expect, it } from 'vitest';
import { isSuperAdmin, parseRole, ROLES, ROLE_LABEL } from '@/auth/roles';

describe('parseRole', () => {
  it('认识两个角色', () => {
    expect(parseRole('super')).toBe('super');
    expect(parseRole('user')).toBe('user');
  });

  it('未知取值一律降级为普通用户', () => {
    // The column is free text, so anything unexpected must not grant privileges.
    for (const raw of ['', 'admin', 'SUPER', ' super', 'root', null]) {
      expect(parseRole(raw)).toBe('user');
    }
  });

  it('每个角色都有标签', () => {
    for (const role of ROLES) {
      expect(ROLE_LABEL[role]).toBeTruthy();
    }
    expect(Object.keys(ROLE_LABEL)).toHaveLength(ROLES.length);
  });
});

describe('isSuperAdmin', () => {
  it('只认 super', () => {
    expect(isSuperAdmin({ id: 1, role: 'super' })).toBe(true);
    expect(isSuperAdmin({ id: 1, role: 'user' })).toBe(false);
  });

  it('没有主体时为 false', () => {
    expect(isSuperAdmin(undefined)).toBe(false);
    expect(isSuperAdmin(null)).toBe(false);
  });
});
