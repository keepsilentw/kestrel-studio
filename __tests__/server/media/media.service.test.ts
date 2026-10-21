import { describe, expect, it } from 'vitest';
import {
  assertImageModel,
  DEFAULT_IMAGE_MODEL,
  normalizeTaskState,
} from '@/media/media.service';

/**
 * Only the pure helpers are covered here. The rest of MediaService talks to the
 * provider over HTTP and writes to disk; that is deliberately out of scope for
 * unit tests, per the boundary documented in vitest.config.mts.
 */
describe('DEFAULT_IMAGE_MODEL', () => {
  it('默认模型本身必须受支持', () => {
    expect(() => assertImageModel(DEFAULT_IMAGE_MODEL)).not.toThrow();
  });
});

describe('assertImageModel', () => {
  it('接受受支持的模型', () => {
    expect(() => assertImageModel('wan2.7-image')).not.toThrow();
    expect(() => assertImageModel('wan2.7-image-pro')).not.toThrow();
  });

  it('拒绝未支持的模型', () => {
    expect(() => assertImageModel('gpt-image-1')).toThrow(/Unsupported image model/);
  });

  it('错误信息里列出受支持模型，便于排障', () => {
    expect(() => assertImageModel('nope')).toThrow('wan2.7-image-pro');
  });

  it('不做大小写归一化', () => {
    expect(() => assertImageModel('WAN2.7-IMAGE')).toThrow();
  });

  it('空字符串与近似名都被拒', () => {
    expect(() => assertImageModel('')).toThrow();
    expect(() => assertImageModel('wan2.7')).toThrow();
    expect(() => assertImageModel('wan2.7-image-pro-plus')).toThrow();
  });
});

describe('normalizeTaskState', () => {
  it('进行中的状态各自映射', () => {
    expect(normalizeTaskState('PENDING', false)).toBe('pending');
    expect(normalizeTaskState('RUNNING', false)).toBe('running');
  });

  it('成功且拿到视频 URL 才算成功', () => {
    expect(normalizeTaskState('SUCCEEDED', true)).toBe('succeeded');
  });

  it('成功但没有视频 URL 判为终态失败，而不是继续轮询', () => {
    // The render is over; there is nothing left to wait for. Polling it forever
    // would hold the task open until MAX_TASK_AGE_MS and then report a timeout,
    // hiding the real reason.
    expect(normalizeTaskState('SUCCEEDED', false)).toBe('failed');
  });

  it('不认识的状态一律判为失败', () => {
    expect(normalizeTaskState('FAILED', false)).toBe('failed');
    expect(normalizeTaskState('CANCELED', false)).toBe('failed');
    expect(normalizeTaskState('', false)).toBe('failed');
  });

  it('不做大小写归一化 —— 小写状态名会被判为失败', () => {
    // Pinned deliberately: the provider's status is matched case-sensitively, so
    // a change on their side would surface as "everything fails" rather than as
    // a silently mis-parsed state.
    expect(normalizeTaskState('succeeded', true)).toBe('failed');
    expect(normalizeTaskState('running', false)).toBe('failed');
  });
});
