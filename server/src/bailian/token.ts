import { readFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';
import Database from 'better-sqlite3';

const DEFAULT_PROVIDER_ID = 'bailian-token-plan';

function resolveProviderId(): string {
  const fromEnv = process.env.BAILIAN_PROVIDER_ID;
  return fromEnv !== undefined && fromEnv.trim().length > 0 ? fromEnv.trim() : DEFAULT_PROVIDER_ID;
}

/**
 * Read the token from cc-switch's own database.
 *
 * This is the durable source: switching Codex to another provider rewrites
 * config.toml wholesale and drops the bailian-token-plan block, but the
 * provider row - and therefore the key - survives in cc-switch's database.
 */
function tokenFromCcSwitchDb(): string | null {
  const dbPath = process.env.CC_SWITCH_DB ?? join(homedir(), '.cc-switch', 'cc-switch.db');
  try {
    const db = new Database(dbPath, { readonly: true, fileMustExist: true });
    try {
      const row = db
        .prepare(
          "SELECT json_extract(settings_config, '$.auth.OPENAI_API_KEY') AS key " +
            "FROM providers WHERE id = ? AND app_type = 'codex'",
        )
        .get(resolveProviderId()) as { key?: unknown } | undefined;
      const value = row?.key;
      return typeof value === 'string' && value.trim().length > 0 ? value.trim() : null;
    } finally {
      db.close();
    }
  } catch {
    return null;
  }
}

/**
 * Fallback: read the bearer token cc-switch wrote into the Codex provider
 * block of ~/.codex/config.toml.
 */
function tokenFromCodexConfig(): string | null {
  try {
    const text = readFileSync(join(homedir(), '.codex', 'config.toml'), 'utf8');
    const header = `[model_providers.${resolveProviderId()}]`;
    const start = text.indexOf(header);
    if (start === -1) {
      return null;
    }
    const rest = text.slice(start + header.length);
    const nextSection = rest.search(/^\[/m);
    const block = nextSection === -1 ? rest : rest.slice(0, nextSection);
    const match = /experimental_bearer_token\s*=\s*"([^"]+)"/.exec(block);
    return match ? match[1] : null;
  } catch {
    return null;
  }
}

export function resolveApiKey(): string {
  const fromEnv = process.env.BAILIAN_API_KEY ?? process.env.DASHSCOPE_API_KEY;
  if (fromEnv !== undefined && fromEnv.trim().length > 0) {
    return fromEnv.trim();
  }
  const fromDb = tokenFromCcSwitchDb();
  if (fromDb !== null) {
    return fromDb;
  }
  const fromConfig = tokenFromCodexConfig();
  if (fromConfig !== null && fromConfig.trim().length > 0) {
    return fromConfig.trim();
  }
  throw new Error(
    'No Bailian API key found. Set BAILIAN_API_KEY, or keep the bailian-token-plan provider in cc-switch.',
  );
}
