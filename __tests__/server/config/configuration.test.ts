import { join } from 'node:path';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { loadConfig } from '@/config/configuration';

/**
 * Pins the deployment contract: every default here is also what the container
 * falls back to when a variable is absent, and two of them (PORT, and the
 * trailing-slash normalisation of the base URLs) have already caused incidents
 * in the field. See docs/deployment.md §2 and src/common/signed-url.ts.
 */
const ENV_KEYS = [
  'PORT',
  'NODE_ENV',
  'SESSION_SECRET',
  'DATABASE_FILE',
  'STORAGE_DIR',
  'PUBLIC_BASE_URL',
  'SUPER_ADMIN_USERNAME',
  'SUPER_ADMIN_PASSWORD',
  'BAILIAN_BASE_URL',
  'BAILIAN_CHAT_MODEL',
  'BAILIAN_REASONING_EFFORT',
  'BAILIAN_VIDEO_MODEL_T2V',
  'BAILIAN_VIDEO_MODEL_I2V',
] as const;

const saved = new Map<string, string | undefined>();

beforeEach(() => {
  for (const key of ENV_KEYS) {
    saved.set(key, process.env[key]);
    delete process.env[key];
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

describe('loadConfig — 默认值', () => {
  it('全部缺省时给出一组可用默认值', () => {
    const config = loadConfig();
    expect(config.port).toBe(8848);
    expect(config.isProduction).toBe(false);
    expect(config.sessionSecret).toMatch(/^[0-9a-f]{64}$/);
    expect(config.databaseFile).toBe(join('data', 'kestrel-studio.db'));
    expect(config.storageDir).toBe('storage');
    expect(config.publicBaseUrl).toBe('https://try.kestrel.justwork.link');
    // No super admin without credentials — the repository must not describe a
    // working login for a deployed instance.
    expect(config.superAdmin).toBeNull();
    expect(config.bailian.baseUrl).toBe('https://token-plan.cn-beijing.maas.aliyuncs.com');
    expect(config.bailian.chatModel).toBe('deepseek-v4.1-flash');
    expect(config.bailian.reasoningEffort).toBe('high');
    expect(config.bailian.videoModelT2v).toBe('happyhorse-1.1-t2v');
    expect(config.bailian.videoModelI2v).toBe('happyhorse-1.1-i2v');
  });

  it('空字符串等同于未设置', () => {
    process.env.PORT = '   ';
    process.env.STORAGE_DIR = '';
    const config = loadConfig();
    expect(config.port).toBe(8848);
    expect(config.storageDir).toBe('storage');
  });
});

describe('loadConfig — PORT', () => {
  it('接受合法端口', () => {
    process.env.PORT = '3000';
    expect(loadConfig().port).toBe(3000);
  });

  it('容忍两端空白', () => {
    process.env.PORT = ' 9000 ';
    expect(loadConfig().port).toBe(9000);
  });

  it.each(['abc', '0', '-1', '3000.5', 'NaN'])('非法值 %s 回退到 8848', (value) => {
    process.env.PORT = value;
    expect(loadConfig().port).toBe(8848);
  });
});

describe('loadConfig — BAILIAN_REASONING_EFFORT', () => {
  it.each(['minimal', 'low', 'medium', 'high'])('接受 %s', (value) => {
    process.env.BAILIAN_REASONING_EFFORT = value;
    expect(loadConfig().bailian.reasoningEffort).toBe(value);
  });

  it('大小写与空白不敏感', () => {
    process.env.BAILIAN_REASONING_EFFORT = '  HIGH ';
    expect(loadConfig().bailian.reasoningEffort).toBe('high');
  });

  it('非法档位回退到 high，而不是报错或降级到 minimal', () => {
    // Falling back to the cheapest tier would silently empty out the thinking
    // panel, which is the feature this setting exists to drive.
    process.env.BAILIAN_REASONING_EFFORT = 'extreme';
    expect(loadConfig().bailian.reasoningEffort).toBe('high');
  });
});

describe('loadConfig — 尾斜杠归一化', () => {
  it('PUBLIC_BASE_URL 去掉尾斜杠', () => {
    // signed-url.ts concatenates this with "/api/assets/...", so a trailing
    // slash would produce a double slash in a URL the provider has to fetch.
    process.env.PUBLIC_BASE_URL = 'https://frames.example.test/';
    expect(loadConfig().publicBaseUrl).toBe('https://frames.example.test');

    process.env.PUBLIC_BASE_URL = 'https://frames.example.test///';
    expect(loadConfig().publicBaseUrl).toBe('https://frames.example.test');
  });

  it('BAILIAN_BASE_URL 去掉尾斜杠', () => {
    process.env.BAILIAN_BASE_URL = 'https://example.test/base/';
    expect(loadConfig().bailian.baseUrl).toBe('https://example.test/base');
  });

  it('没有尾斜杠时原样保留', () => {
    process.env.PUBLIC_BASE_URL = 'https://frames.example.test';
    expect(loadConfig().publicBaseUrl).toBe('https://frames.example.test');
  });
});

describe('loadConfig — 每次调用都重新读环境', () => {
  it('不缓存，改环境后立刻生效', () => {
    // The tests below rely on this; so does deploy.sh, which writes .env and
    // restarts the container rather than reloading anything in-process.
    expect(loadConfig().port).toBe(8848);
    process.env.PORT = '1234';
    expect(loadConfig().port).toBe(1234);
  });
});

describe('loadConfig — SESSION_SECRET', () => {
  it('非 production 缺省时随机值在本进程内保持稳定', () => {
    // Signing happens in one loadConfig() call and verification in another; a
    // value that changed in between would reject every i2v frame.
    const first = loadConfig().sessionSecret;
    expect(first).toMatch(/^[0-9a-f]{64}$/);
    expect(loadConfig().sessionSecret).toBe(first);
  });

  it('给了就用，原样取值', () => {
    process.env.SESSION_SECRET = ' a-fixed-secret ';
    expect(loadConfig().sessionSecret).toBe('a-fixed-secret');
  });

  it('production 缺省直接失败，而不是回落到可猜的值', () => {
    process.env.NODE_ENV = 'production';
    expect(() => loadConfig()).toThrow('SESSION_SECRET must be set when NODE_ENV=production');
  });

  it('production 给了值就正常', () => {
    process.env.NODE_ENV = 'production';
    process.env.SESSION_SECRET = 'from-the-host-env-file';
    expect(loadConfig().sessionSecret).toBe('from-the-host-env-file');
  });
});

describe('loadConfig — NODE_ENV', () => {
  it('production 打开生产闸门，其它值都不算', () => {
    // The gate decides whether the built-in convenience account is created, so
    // only the exact value the Dockerfile sets may trip it.
    // SESSION_SECRET is supplied because production otherwise refuses to boot.
    process.env.NODE_ENV = 'production';
    process.env.SESSION_SECRET = 'gate-check-secret';
    expect(loadConfig().isProduction).toBe(true);

    process.env.NODE_ENV = 'Production';
    expect(loadConfig().isProduction).toBe(false);
  });
});

describe('loadConfig — SUPER_ADMIN_*', () => {
  it('两个变量都给全时注入凭据', () => {
    process.env.SUPER_ADMIN_USERNAME = ' operator-root ';
    process.env.SUPER_ADMIN_PASSWORD = ' a-long-one ';
    expect(loadConfig().superAdmin).toEqual({ username: 'operator-root', password: 'a-long-one' });
  });

  it.each([
    ['只给账号名', 'operator-root', ''],
    ['只给口令', '', 'a-long-one'],
    ['两个都空白', '  ', '  '],
  ])('%s 视为未配置，不建超管', (_label, username, password) => {
    // Half-configured is worse than not configured: a /admin with nobody able
    // to reach it, or a super admin with an empty password.
    process.env.SUPER_ADMIN_USERNAME = username;
    process.env.SUPER_ADMIN_PASSWORD = password;
    expect(loadConfig().superAdmin).toBeNull();
  });
});
