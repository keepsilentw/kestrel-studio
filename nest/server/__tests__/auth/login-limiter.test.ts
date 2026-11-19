import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
  LOGIN_WINDOW_MS,
  LoginLimiter,
  MAX_IDENTITY_FAILURES,
  MAX_IP_FAILURES,
} from '@/auth/login-limiter.service';

describe('LoginLimiter', () => {
  let limiter: LoginLimiter;

  beforeEach(() => {
    limiter = new LoginLimiter();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('同一账号在同一地址连续失败后被封禁', () => {
    for (let index = 0; index < MAX_IDENTITY_FAILURES; index += 1) {
      expect(limiter.blockedFor('203.0.113.7', 'admin')).toBeNull();
      limiter.recordFailure('203.0.113.7', 'admin');
    }
    expect(limiter.blockedFor('203.0.113.7', 'admin')).not.toBeNull();
    expect(limiter.blockedFor('203.0.113.7', 'someone-else')).toBeNull();
  });

  it('登录成功会清掉账号维度，但保留地址维度', () => {
    for (let index = 0; index < MAX_IDENTITY_FAILURES - 1; index += 1) {
      limiter.recordFailure('203.0.113.7', 'admin');
    }
    limiter.recordSuccess('203.0.113.7', 'admin');
    limiter.recordFailure('203.0.113.7', 'admin');
    expect(limiter.blockedFor('203.0.113.7', 'admin')).toBeNull();
  });

  it('轮换用户名仍会撞上地址维度的上限', () => {
    for (let index = 0; index < MAX_IP_FAILURES; index += 1) {
      limiter.recordFailure('203.0.113.7', `user-${index}`);
    }
    expect(limiter.blockedFor('203.0.113.7', 'one-more')).not.toBeNull();
    expect(limiter.blockedFor('203.0.113.8', 'one-more')).toBeNull();
  });

  it('窗口过期后重新放行', () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date('2026-11-19T00:00:00Z'));
    limiter.recordFailure('203.0.113.7', 'admin');
    vi.advanceTimersByTime(LOGIN_WINDOW_MS + 1);
    expect(limiter.blockedFor('203.0.113.7', 'admin')).toBeNull();
  });
});
