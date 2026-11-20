import { randomBytes } from 'node:crypto';
import { join } from 'node:path';

const DEFAULT_BASE_URL = 'https://token-plan.cn-beijing.maas.aliyuncs.com';
const DEFAULT_CHAT_MODEL = 'deepseek-v4.1-flash';
const DEFAULT_VIDEO_MODEL = 'wan3.0-video';
const DEFAULT_VOICE_MODEL = 'qwen-audio-3.0-realtime-plus';

/**
 * Used to build the signed, publicly reachable frame URL that image-to-video
 * hands to the provider (the provider fetches it itself, so a localhost value
 * makes the first-frame mode unusable — the video tool rejects it explicitly).
 */
const DEFAULT_PUBLIC_BASE_URL = 'https://try.kestrel.justwork.link';

const REASONING_EFFORTS = ['minimal', 'low', 'medium', 'high'] as const;
export type ReasoningEffort = (typeof REASONING_EFFORTS)[number];
const DEFAULT_REASONING_EFFORT: ReasoningEffort = 'high';
/** Per-account media budget; `0` disables the check. */
const DEFAULT_ASSET_QUOTA_BYTES = 2 * 1024 * 1024 * 1024;
/**
 * Turns one account may have in flight at once; `0` disables the check. One
 * conversation already allows only one turn (409); this closes the "open N
 * conversations" multiplication, which is the remaining way to make N paid model
 * calls at the same time. Kept low on purpose: every turn is billed.
 */
const DEFAULT_MAX_TURNS_PER_USER = 2;
const MAX_TURNS_PER_USER_CEILING = 16;
/**
 * Generated assets per account per trailing 24 hours; `0` disables it. The burst
 * guard is `MAX_TURNS_PER_USER`; this is the volume guard, so a script that spaces
 * its calls out still stops. Generous on purpose.
 */
const DEFAULT_DAILY_ASSET_LIMIT = 200;
const MAX_DAILY_ASSET_LIMIT = 10_000;
/**
 * Device token lifetime in days; `0` means no expiry. Rows with a NULL expiry are
 * backfilled at boot while this is non-zero, so turning it on reaches tokens that
 * predate the column.
 */
const DEFAULT_TOKEN_TTL_DAYS = 90;
const MAX_TOKEN_TTL_DAYS = 3650;

export interface BailianConfig {
  baseUrl: string;
  chatModel: string;
  reasoningEffort: ReasoningEffort;
  /**
   * One model serves both text-to-video and image-to-video: `wan3.0-video`
   * branches on whether `input.media` carries a first frame, so there is no
   * t2v/i2v pair to configure any more.
   */
  videoModel: string;
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
  assetQuotaBytes: number;
  /** In-flight turns one account may have; `0` disables the check. */
  maxTurnsPerUser: number;
  /** Generated assets per account per trailing 24 hours; `0` disables it. */
  dailyAssetLimit: number;
  /** Device token lifetime in days; `0` means no expiry. */
  tokenTtlDays: number;
  /**
   * One access line per request (`common/access-log.ts`). Off unless asked for, so
   * nobody turns production log volume up by accident — the Rust server gates the
   * same thing behind `RUST_LOG=debug`, and a forgotten debug arm grew that log to
   * 16 MB in 2.8 days (docs/verification.md §21, docs/deployment.md §2.1).
   */
  accessLog: boolean;
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
    return 9949;
  }
  const parsed = Number(raw);
  return Number.isInteger(parsed) && parsed > 0 ? parsed : 9949;
}

function readAssetQuota(): number {
  const raw = readEnv('ASSET_QUOTA_BYTES');
  if (raw === null) {
    return DEFAULT_ASSET_QUOTA_BYTES;
  }
  const parsed = Number(raw);
  return Number.isInteger(parsed) && parsed >= 0 ? parsed : DEFAULT_ASSET_QUOTA_BYTES;
}

/** `0` is meaningful (no expiry), so it is not clamped up to 1. */
function readTokenTtlDays(): number {
  const raw = readEnv('TOKEN_TTL_DAYS');
  if (raw === null) {
    return DEFAULT_TOKEN_TTL_DAYS;
  }
  const parsed = Number(raw);
  if (!Number.isInteger(parsed) || parsed < 0) {
    return DEFAULT_TOKEN_TTL_DAYS;
  }
  return Math.min(parsed, MAX_TOKEN_TTL_DAYS);
}

/** `0` is meaningful (off), so it is not clamped up to 1. */
function readDailyAssetLimit(): number {
  const raw = readEnv('DAILY_ASSET_LIMIT');
  if (raw === null) {
    return DEFAULT_DAILY_ASSET_LIMIT;
  }
  const parsed = Number(raw);
  if (!Number.isInteger(parsed) || parsed < 0) {
    return DEFAULT_DAILY_ASSET_LIMIT;
  }
  return Math.min(parsed, MAX_DAILY_ASSET_LIMIT);
}

/** `0` is meaningful (off), so it is not clamped up to 1. */
function readMaxTurnsPerUser(): number {
  const raw = readEnv('MAX_TURNS_PER_USER');
  if (raw === null) {
    return DEFAULT_MAX_TURNS_PER_USER;
  }
  const parsed = Number(raw);
  if (!Number.isInteger(parsed) || parsed < 0) {
    return DEFAULT_MAX_TURNS_PER_USER;
  }
  return Math.min(parsed, MAX_TURNS_PER_USER_CEILING);
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
 * `ACCESS_LOG=1` (or `true`, any case). Anything else — including a blank value —
 * keeps the access log off, which is also what a normal deploy gets.
 */
function readAccessLog(): boolean {
  const raw = readEnv('ACCESS_LOG');
  return raw !== null && (raw === '1' || raw.toLowerCase() === 'true');
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
    assetQuotaBytes: readAssetQuota(),
    maxTurnsPerUser: readMaxTurnsPerUser(),
    dailyAssetLimit: readDailyAssetLimit(),
    tokenTtlDays: readTokenTtlDays(),
    accessLog: readAccessLog(),
    superAdmin: readSuperAdmin(),
    bailian: {
      baseUrl: (readEnv('BAILIAN_BASE_URL') ?? DEFAULT_BASE_URL).replace(/\/+$/, ''),
      chatModel: readEnv('BAILIAN_CHAT_MODEL') ?? DEFAULT_CHAT_MODEL,
      reasoningEffort: readReasoningEffort(),
      videoModel: readEnv('BAILIAN_VIDEO_MODEL') ?? DEFAULT_VIDEO_MODEL,
      voiceModel: readEnv('BAILIAN_VOICE_MODEL') ?? DEFAULT_VOICE_MODEL,
    },
  };
}
