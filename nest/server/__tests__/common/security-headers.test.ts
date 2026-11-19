import { describe, expect, it } from 'vitest';
import { CONTENT_SECURITY_POLICY } from '@/common/security-headers';

describe('CONTENT_SECURITY_POLICY', () => {
  it('外部加载器之后不再需要 inline 脚本或样式例外', () => {
    expect(CONTENT_SECURITY_POLICY).toContain(
      "script-src 'self' 'wasm-unsafe-eval'",
    );
    expect(CONTENT_SECURITY_POLICY).toContain("style-src 'self'");
    expect(CONTENT_SECURITY_POLICY).not.toContain('unsafe-inline');
  });
});
