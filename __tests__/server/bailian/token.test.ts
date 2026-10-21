import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { resolveApiKey } from '@/bailian/token';

const ENV_KEYS = ['BAILIAN_API_KEY', 'DASHSCOPE_API_KEY'] as const;
const saved = new Map<string, string | undefined>();

beforeEach(() => {
  for (const key of ENV_KEYS) {
    saved.set(key, process.env[key]);
  }
});

afterEach(() => {
  for (const key of ENV_KEYS) {
    const value = saved.get(key);
    if (value === undefined) {
      delete process.env[key];
    } else {
      process.env[key] = value;
    }
  }
  saved.clear();
});

/**
 * Only the environment-variable stage is covered. The two remaining stages —
 * cc-switch's SQLite database and ~/.codex/config.toml — read files that exist
 * on this machine and hold a real key, so asserting on them here would be
 * environment-dependent. They are exercised by the running app instead.
 *
 * Quirk worth knowing (not covered, for the same reason): because the lookup is
 * `BAILIAN_API_KEY ?? DASHSCOPE_API_KEY`, a *blank* BAILIAN_API_KEY does not fall
 * through to DASHSCOPE_API_KEY. The `??` selects it, the trim check then rejects
 * it, and control passes to the cc-switch database. Setting the variable to ""
 * is therefore not the same as leaving it unset.
 */
describe('resolveApiKey — 环境变量优先级', () => {
  it('BAILIAN_API_KEY 优先', () => {
    process.env.BAILIAN_API_KEY = 'bailian-key';
    expect(resolveApiKey()).toBe('bailian-key');
  });

  it('DASHSCOPE_API_KEY 作为别名', () => {
    delete process.env.BAILIAN_API_KEY;
    process.env.DASHSCOPE_API_KEY = 'dashscope-key';
    expect(resolveApiKey()).toBe('dashscope-key');
  });

  it('两个都在时 BAILIAN 胜出', () => {
    process.env.BAILIAN_API_KEY = 'bailian-key';
    process.env.DASHSCOPE_API_KEY = 'dashscope-key';
    expect(resolveApiKey()).toBe('bailian-key');
  });

  it('去掉两端空白', () => {
    process.env.BAILIAN_API_KEY = '  bailian-key  ';
    expect(resolveApiKey()).toBe('bailian-key');
  });
});
