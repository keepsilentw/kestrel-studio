import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

/** What a load attempt did, so callers and tests can assert without logging. */
export interface EnvFileLoadResult {
  path: string;
  /** False when the file does not exist, which is the normal case in a container. */
  present: boolean;
  /** Keys written into `process.env`. */
  applied: number;
  /** Lines dropped: no key, no `=`, or the key was already in the environment. */
  skipped: number;
}

function unquote(value: string): string {
  const quoted =
    value.length >= 2 &&
    ((value.startsWith('"') && value.endsWith('"')) ||
      (value.startsWith("'") && value.endsWith("'")));
  return quoted ? value.slice(1, -1) : value;
}

/**
 * Overlays `./.env` onto `process.env` before anything reads the environment.
 *
 * Not `@nestjs/config`: its `ConfigModule` populates the environment during
 * module initialisation, while this project calls `loadConfig()` directly from
 * the entry point and from provider factories. Anything that must already be in
 * `process.env` at the first `loadConfig()` cannot come from a module lifecycle
 * hook. See `src/main.ts`, which calls this before loading any configuration.
 *
 * Rules, in the order that matters:
 *   * the real environment wins — an existing variable is never overwritten, so
 *     the container's `env_file` and a shell export both take precedence;
 *   * a missing file is not an error (CI, the image, a fresh clone);
 *   * only `KEY=VALUE` lines count, split at the first `=`, and the value keeps
 *     everything after it — including a `#`, so a password with one survives.
 */
export function loadEnvFile(directory: string = process.cwd()): EnvFileLoadResult {
  const path = resolve(directory, '.env');
  let text: string;
  try {
    text = readFileSync(path, 'utf8');
  } catch {
    return { path, present: false, applied: 0, skipped: 0 };
  }

  let applied = 0;
  let skipped = 0;
  for (const line of text.split(/\r?\n/)) {
    const trimmed = line.trim();
    if (trimmed === '' || trimmed.startsWith('#')) {
      continue;
    }
    const separator = trimmed.indexOf('=');
    if (separator < 1) {
      skipped += 1;
      continue;
    }
    const key = trimmed.slice(0, separator).trim();
    if (key === '') {
      skipped += 1;
      continue;
    }
    if (process.env[key] !== undefined) {
      skipped += 1;
      continue;
    }
    process.env[key] = unquote(trimmed.slice(separator + 1).trim());
    applied += 1;
  }

  return { path, present: true, applied, skipped };
}
