import { randomBytes } from 'node:crypto';
import { join } from 'node:path';

const DEFAULT_BASE_URL = 'https://token-plan.cn-beijing.maas.aliyuncs.com';
const DEFAULT_CHAT_MODEL = 'deepseek-v4.1-flash';
const DEFAULT_T2V_MODEL = 'happyhorse-1.1-t2v';
const DEFAULT_I2V_MODEL = 'happyhorse-1.1-i2v';
const DEFAULT_VOICE_MODEL = 'qwen-audio-3.0-realtime-plus';

/**
 * Used to build the signed, publicly reachable frame URL that image-to-video
 * hands to the provider (the provider fetches it itself, so a localhost value
 * makes i2v unusable — the video tool rejects it explicitly).
 */
const DEFAULT_PUBLIC_BASE_URL = 'https://try.kestrel.justwork.link';

const REASONING_EFFORTS = ['minimal', 'low', 'medium', 'high'] as const;
export type ReasoningEffort = (typeof REASONING_EFFORTS)[number];
const DEFAULT_REASONING_EFFORT: ReasoningEffort = 'high';

export interface BailianConfig {
  baseUrl: string;
  chatModel: string;
  reasoningEffort: ReasoningEffort;
  videoModelT2v: string;
  videoModelI2v: string;
  /** The realtime speech model behind /voice. */
  voiceModel: string;
}

/**
 * Credentials for the bootstrap super admin. Both halves come from the
 * environment so the repository carries no working login for a deployed
 * instance.
 */
export interface SeedAccount {
  username: string;
  password: string;
}

export interface AppConfig {
  port: number;
  /**
   * `NODE_ENV=production` (set by the Dockerfile). It gates the convenience
   * account: a published repository must not describe a credential that also
   * works against a deployed instance.
   */
  isProduction: boolean;
  sessionSecret: string;
  databaseFile: string;
  storageDir: string;
  publicBaseUrl: string;
  /** Null when either `SUPER_ADMIN_*` variable is absent — then no super admin is seeded. */
  superAdmin: SeedAccount | null;
  bailian: BailianConfig;
}

function readEnv(key: string): string | null {
  const value = process.env[key];
  return value !== undefined && value.trim().length > 0 ? value.trim() : null;
}

function readPort(): number {
  const raw = readEnv('PORT');
  if (raw === null) {
    return 8848;
  }
  const parsed = Number(raw);
  return Number.isInteger(parsed) && parsed > 0 ? parsed : 8848;
}

/**
 * Defaults to `high`. Measured against the live endpoint: with the provider's
 * default effort an image request produced ~3 reasoning deltas (the model went
 * straight for the tool call), while `high` produced >1300 for a comparable
 * request. The thinking panel is only worth showing at the higher setting.
 */
function readReasoningEffort(): ReasoningEffort {
  const raw = readEnv('BAILIAN_REASONING_EFFORT');
  if (raw === null) {
    return DEFAULT_REASONING_EFFORT;
  }
  const normalized = raw.trim().toLowerCase();
  for (const effort of REASONING_EFFORTS) {
    if (effort === normalized) {
      return effort;
    }
  }
  return DEFAULT_REASONING_EFFORT;
}

/**
 * Fallback for a missing `SESSION_SECRET` outside production. The same value
 * signs sessions and asset frame URLs, so a published constant would let anyone
 * forge either. It is generated once per process because
 * `buildSignedFrameUrl()` signs inside one `loadConfig()` call and
 * `verifySignedFrame()` checks inside another: a per-call value would reject
 * every image-to-video frame. Restarting logs local sessions out, which is the
 * price of never shipping a guessable secret.
 */
let generatedSessionSecret: string | null = null;

/** Production requires an explicit value; local boots use the cached random one. */
function readSessionSecret(isProduction: boolean): string {
  const fromEnv = readEnv('SESSION_SECRET');
  if (fromEnv !== null) {
    return fromEnv;
  }
  if (isProduction) {
    throw new Error('SESSION_SECRET must be set when NODE_ENV=production');
  }
  generatedSessionSecret ??= randomBytes(32).toString('hex');
  return generatedSessionSecret;
}

/**
 * Both variables must be present, and neither may be blank: a half-configured
 * super admin is a locked-out /admin with no way to notice.
 */
function readSuperAdmin(): SeedAccount | null {
  const username = readEnv('SUPER_ADMIN_USERNAME');
  const password = readEnv('SUPER_ADMIN_PASSWORD');
  if (username === null || password === null) {
    return null;
  }
  return { username, password };
}

export function loadConfig(): AppConfig {
  const isProduction = readEnv('NODE_ENV') === 'production';
  return {
    port: readPort(),
    isProduction,
    sessionSecret: readSessionSecret(isProduction),
    databaseFile: readEnv('DATABASE_FILE') ?? join('data', 'kestrel-studio.db'),
    storageDir: readEnv('STORAGE_DIR') ?? 'storage',
    publicBaseUrl: (readEnv('PUBLIC_BASE_URL') ?? DEFAULT_PUBLIC_BASE_URL).replace(/\/+$/, ''),
    superAdmin: readSuperAdmin(),
    bailian: {
      baseUrl: (readEnv('BAILIAN_BASE_URL') ?? DEFAULT_BASE_URL).replace(/\/+$/, ''),
      chatModel: readEnv('BAILIAN_CHAT_MODEL') ?? DEFAULT_CHAT_MODEL,
      reasoningEffort: readReasoningEffort(),
      videoModelT2v: readEnv('BAILIAN_VIDEO_MODEL_T2V') ?? DEFAULT_T2V_MODEL,
      videoModelI2v: readEnv('BAILIAN_VIDEO_MODEL_I2V') ?? DEFAULT_I2V_MODEL,
      voiceModel: readEnv('BAILIAN_VOICE_MODEL') ?? DEFAULT_VOICE_MODEL,
    },
  };
}
