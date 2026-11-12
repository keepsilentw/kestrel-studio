import {
  Global,
  Inject,
  Logger,
  Module,
  OnApplicationBootstrap,
  OnApplicationShutdown,
} from '@nestjs/common';
import Database from 'better-sqlite3';
import { drizzle, type BetterSQLite3Database } from 'drizzle-orm/better-sqlite3';
import { mkdirSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { loadConfig } from '@/config/configuration';
import { ensureAdminUser, ensureSuperAdmin } from './seed';
import * as schema from './schema';

export const SQLITE_CONNECTION = Symbol('SQLITE_CONNECTION');
export const DRIZZLE_INSTANCE = Symbol('DRIZZLE_INSTANCE');

export type DrizzleDb = BetterSQLite3Database<typeof schema>;

/** Schema repair reports here: it runs before the Nest logger context exists. */
const schemaLogger = new Logger('Schema');

/**
 * Initial schema. Kept in sync with schema.ts by hand; schema changes that need
 * data migration should go through drizzle-kit (`pnpm db:push`) instead.
 */
const DDL = `
CREATE TABLE IF NOT EXISTS users (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  username TEXT NOT NULL UNIQUE,
  password_hash TEXT NOT NULL,
  role TEXT NOT NULL DEFAULT 'user',
  created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS conversations (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  user_id INTEGER NOT NULL REFERENCES users(id),
  title TEXT NOT NULL,
  deleted_at INTEGER,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS messages (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  conversation_id INTEGER NOT NULL REFERENCES conversations(id),
  role TEXT NOT NULL,
  content TEXT NOT NULL,
  reasoning TEXT,
  tool_calls TEXT,
  -- Which backend wrote the row (nest / rust). The boot recovery of abandoned turns
  -- keys off it, so a restart of one stack cannot end the other's live turn. NULL
  -- means a row predating the column, see abandonStalePlaceholders.
  -- (No backticks anywhere in this string: it is a JS template literal.)
  owner TEXT,
  created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS assets (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  message_id INTEGER NOT NULL REFERENCES messages(id),
  kind TEXT NOT NULL,
  file_path TEXT NOT NULL,
  source_url TEXT,
  mime TEXT NOT NULL,
  bytes INTEGER NOT NULL,
  -- Pixel dimensions of the stored bytes, read from the file's own header at write
  -- time. Nullable: a video, an unreadable header, or a row predating the column all
  -- mean the img gets no width/height, never that one is invented.
  width INTEGER,
  height INTEGER,
  created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS conversation_events (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  conversation_id INTEGER NOT NULL REFERENCES conversations(id),
  seq INTEGER NOT NULL,
  message_id INTEGER,
  type TEXT NOT NULL,
  payload TEXT NOT NULL,
  created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS generation_tasks (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  conversation_id INTEGER NOT NULL REFERENCES conversations(id),
  message_id INTEGER,
  kind TEXT NOT NULL,
  provider_task_id TEXT NOT NULL,
  model TEXT NOT NULL,
  prompt TEXT NOT NULL,
  params TEXT,
  status TEXT NOT NULL,
  attempts INTEGER NOT NULL DEFAULT 0,
  -- Lease: which backend is advancing this render right now, and since when. The
  -- worker claims a row with one conditional UPDATE before it touches the provider,
  -- so two stacks cannot poll, mirror and journal the same task twice.
  claimed_by TEXT,
  claimed_at INTEGER,
  error TEXT,
  asset_id INTEGER,
  result_message_id INTEGER,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  finished_at INTEGER
);

CREATE INDEX IF NOT EXISTS idx_conversations_user ON conversations(user_id, updated_at DESC);
CREATE INDEX IF NOT EXISTS idx_messages_conversation ON messages(conversation_id, id);
CREATE INDEX IF NOT EXISTS idx_assets_message ON assets(message_id);
CREATE INDEX IF NOT EXISTS idx_tasks_active ON generation_tasks(status, updated_at);
CREATE INDEX IF NOT EXISTS idx_tasks_conversation ON generation_tasks(conversation_id, id);
-- This one has to be UNIQUE, because seq is the stream cursor and two writers
-- computing the same value would otherwise both land, after which the seq > filter
-- hides the loser's row from live delivery and from replay alike. Plain here on
-- purpose: ensureUniqueEventSeq is what makes it unique, and it repairs the rows a
-- non-unique era left behind first. Declaring it UNIQUE in this string would instead
-- refuse to boot on such a database.
CREATE INDEX IF NOT EXISTS idx_events_conversation ON conversation_events(conversation_id, seq);

-- Revocable long-lived credentials for non-browser hosts (docs/roadmap.md §5.1).
-- Only the SHA-256 of the token is stored: this file is a plain backup target,
-- and a leaked backup must not equal a working credential on every device.
CREATE TABLE IF NOT EXISTS api_tokens (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  user_id INTEGER NOT NULL REFERENCES users(id),
  label TEXT NOT NULL,
  token_hash TEXT NOT NULL UNIQUE,
  created_at INTEGER NOT NULL,
  last_seen_at INTEGER,
  revoked_at INTEGER
);
CREATE INDEX IF NOT EXISTS idx_api_tokens_user ON api_tokens(user_id, revoked_at);
`;

/**
 * `CREATE TABLE IF NOT EXISTS` cannot add a column to a table that already
 * exists, and this database file survives deploys (it is a docker volume). So
 * columns introduced after the first release are added explicitly here.
 */
const ADDED_COLUMNS: { table: string; column: string; definition: string }[] = [
  // The two dimensions a replay `<img>` reserves its box with. Read from the stored
  // bytes, not from the request: docs/verification.md §40.
  { table: 'assets', column: 'width', definition: 'INTEGER' },
  { table: 'assets', column: 'height', definition: 'INTEGER' },
  { table: 'messages', column: 'mode', definition: 'TEXT' },
  // Every account that existed before roles did lands on the least privileged
  // role; the seeded super admin is created with `super` explicitly.
  { table: 'users', column: 'role', definition: "TEXT NOT NULL DEFAULT 'user'" },
  // Null means live; a timestamp hides the conversation from its owner only.
  { table: 'conversations', column: 'deleted_at', definition: 'INTEGER' },
  // The three columns that make the two backends safe to run side by side
  // (docs/architecture.md §13.2). All nullable: a row written before this migration,
  // or by a stack that has not run it yet, is simply unowned.
  { table: 'messages', column: 'owner', definition: 'TEXT' },
  { table: 'generation_tasks', column: 'claimed_by', definition: 'TEXT' },
  { table: 'generation_tasks', column: 'claimed_at', definition: 'INTEGER' },
];

function ensureColumns(connection: Database.Database): void {
  for (const { table, column, definition } of ADDED_COLUMNS) {
    const existing = connection
      .prepare(`SELECT name FROM pragma_table_info(?)`)
      .all(table) as { name: string }[];
    if (!existing.some((row) => row.name === column)) {
      connection.exec(`ALTER TABLE ${table} ADD COLUMN ${column} ${definition}`);
    }
  }
}

/**
 * Brings a connection up to the current schema, pragmas included.
 *
 * Exported so tests can point the same DDL at an in-memory database. Note that
 * `DATABASE_FILE=:memory:` does NOT work — `openDatabase` runs the value through
 * `resolve()`, so it becomes a real file literally named `:memory:`. Tests must
 * open the connection themselves and call this.
 */
export function applySchema(connection: Database.Database): void {
  connection.pragma('journal_mode = WAL');
  connection.pragma('foreign_keys = ON');
  connection.exec(DDL);
  ensureColumns(connection);
  ensureUniqueEventSeq(connection);
}

/**
 * Makes the journal's cursor index UNIQUE, repairing duplicate `seq` rows first.
 *
 * `seq` is per conversation and is what a reconnecting client resumes from, so a
 * duplicated pair is not cosmetic: live delivery and replay both read `seq >`, so the
 * row that lost the race is invisible to every reader forever. Until 2026-11-09 the
 * value was allocated by a read-then-write that no transaction serialized across
 * processes, and the index was plain (docs/architecture.md §13.2).
 *
 * Idempotent, and runs on every boot: a no-op once the index is unique, which is the
 * normal path. When it is not, the losers of each contested pair move to the tail of
 * that conversation's numbering in rowid order, keeping the lowest row id at the
 * contested value — that is the row cursors have already been served from. Promoting
 * an always-hidden row to the end of the journal is the least destructive reading of
 * an ordering that was never valid to begin with.
 *
 * Loud but not fatal, mirroring `rust/server`'s `enforce_unique_event_seq`: a
 * database whose journal cannot be made unique is a real bug, yet refusing to open it
 * would trade a corrupt stream for no service at all.
 */
function ensureUniqueEventSeq(connection: Database.Database): void {
  const index = connection
    .prepare(
      `SELECT "unique" AS isUnique FROM pragma_index_list('conversation_events') WHERE name = 'idx_events_conversation'`,
    )
    .get() as { isUnique: number } | undefined;
  if (index !== undefined && index.isUnique === 1) {
    return;
  }

  const dups = connection
    .prepare(
      `SELECT e.conversation_id AS conversationId, e.seq AS seq, e.id AS id
       FROM conversation_events e
       JOIN (SELECT conversation_id, seq FROM conversation_events
             GROUP BY conversation_id, seq HAVING COUNT(*) > 1) d
         ON d.conversation_id = e.conversation_id AND d.seq = e.seq
       ORDER BY e.conversation_id ASC, e.seq ASC, e.id ASC`,
    )
    .all() as { conversationId: number; seq: number; id: number }[];

  // One pass, keeping the first rowid of every contested pair. `MAX(seq)` is re-read
  // per assignment so the tail keeps growing even across two groups of one conversation.
  const nextSeq = connection.prepare(
    `SELECT COALESCE(MAX(seq), 0) + 1 AS next FROM conversation_events WHERE conversation_id = ?`,
  );
  const move = connection.prepare(`UPDATE conversation_events SET seq = ? WHERE id = ?`);
  let renumbered = 0;
  let previous: string | null = null;
  for (const row of dups) {
    const pair = `${row.conversationId}:${row.seq}`;
    if (previous === pair) {
      const { next } = nextSeq.get(row.conversationId) as { next: number };
      move.run(next, row.id);
      renumbered += 1;
    }
    previous = pair;
  }

  // Drop-then-create rather than `CREATE UNIQUE INDEX IF NOT EXISTS`: the name is
  // already taken by the plain index the DDL above leaves in place.
  connection.exec('DROP INDEX IF EXISTS idx_events_conversation');
  try {
    connection.exec(
      'CREATE UNIQUE INDEX idx_events_conversation ON conversation_events(conversation_id, seq)',
    );
    if (renumbered > 0) {
      // Worth an ERROR line even though the boot continues: rows no reader could
      // ever have seen just became part of the conversation's history.
      schemaLogger.error(
        `repaired ${renumbered} duplicate journal seq value(s) and made the cursor index UNIQUE`,
      );
    }
  } catch (error: unknown) {
    schemaLogger.error(
      `the journal cursor index is still not UNIQUE - seq allocation is unsafe, see docs/architecture.md §13.2: ${
        error instanceof Error ? error.message : String(error)
      }`,
    );
  }
}

function openDatabase(): Database.Database {
  const { databaseFile } = loadConfig();
  const absolute = resolve(databaseFile);
  mkdirSync(dirname(absolute), { recursive: true });
  const connection = new Database(absolute);
  applySchema(connection);
  return connection;
}

@Global()
@Module({
  providers: [
    {
      provide: SQLITE_CONNECTION,
      useFactory: openDatabase,
    },
    {
      provide: DRIZZLE_INSTANCE,
      useFactory: (connection: Database.Database): DrizzleDb => drizzle(connection, { schema }),
      inject: [SQLITE_CONNECTION],
    },
  ],
  exports: [SQLITE_CONNECTION, DRIZZLE_INSTANCE],
})
export class DatabaseModule implements OnApplicationBootstrap, OnApplicationShutdown {
  constructor(
    @Inject(DRIZZLE_INSTANCE) private readonly db: DrizzleDb,
    @Inject(SQLITE_CONNECTION) private readonly connection: Database.Database,
  ) {}

  async onApplicationBootstrap(): Promise<void> {
    const config = loadConfig();
    // The convenience account is a local first-run affordance. Under
    // NODE_ENV=production (set by the Dockerfile) nothing is created from a
    // credential written in the repository — the only way in is the
    // environment-injected super admin.
    if (!config.isProduction) {
      await ensureAdminUser(this.db);
    }
    await ensureSuperAdmin(this.db, config.superAdmin);
  }

  onApplicationShutdown(): void {
    this.connection.close();
  }
}
