import { describe, expect, it } from 'vitest';
import { acceptsGzip } from '@/common/wasm-gzip';

describe('acceptsGzip', () => {
  it('接受 gzip，拒绝 br 与缺省', () => {
    expect(acceptsGzip('gzip, deflate')).toBe(true);
    expect(acceptsGzip('br')).toBe(false);
    expect(acceptsGzip(undefined)).toBe(false);
  });

  it('q=0 是拒绝，不是接受', () => {
    expect(acceptsGzip('gzip;q=0')).toBe(false);
    expect(acceptsGzip('gzip;q=0.0, deflate')).toBe(false);
  });
});
