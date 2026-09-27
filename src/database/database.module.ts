import { Global, Inject, Module, OnApplicationBootstrap, OnApplicationShutdown } from '@nestjs/common';
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
`;

/**
 * `CREATE TABLE IF NOT EXISTS` cannot add a column to a table that already
 * exists, and this database file survives deploys (it is a docker volume). So
 * columns introduced after the first release are added explicitly here.
 */
const ADDED_COLUMNS: { table: string; column: string; definition: string }[] = [
  { table: 'messages', column: 'mode', definition: 'TEXT' },
  // Every account that existed before roles did lands on the least privileged
  // role; the seeded super admin is created with `super` explicitly.
  { table: 'users', column: 'role', definition: "TEXT NOT NULL DEFAULT 'user'" },
  // Null means live; a timestamp hides the conversation from its owner only.
  { table: 'conversations', column: 'deleted_at', definition: 'INTEGER' },
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
