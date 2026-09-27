import { integer, sqliteTable, text } from 'drizzle-orm/sqlite-core';

/**
 * Login sessions live in their own table managed by better-sqlite3-session-store
 * (table name `sessions`). They are deliberately NOT modelled here, to avoid two
 * writers owning the same table.
 */

export const users = sqliteTable('users', {
  id: integer('id').primaryKey({ autoIncrement: true }),
  username: text('username').notNull().unique(),
  passwordHash: text('password_hash').notNull(),
  /**
   * `super` | `user` (src/auth/roles.ts). Read back through `parseRole()`, which
   * is why the column is not narrowed here: an unknown value must degrade to the
   * least privileged role rather than slip past the type system.
   */
  role: text('role').notNull(),
  createdAt: integer('created_at', { mode: 'timestamp_ms' }).notNull(),
});

export const conversations = sqliteTable('conversations', {
  id: integer('id').primaryKey({ autoIncrement: true }),
  userId: integer('user_id')
    .notNull()
    .references(() => users.id),
  title: text('title').notNull(),
  /**
   * Soft delete: the history dropdown hides it from its owner, and nothing is
   * removed — a super admin still reads the whole conversation. Null while live.
   */
  deletedAt: integer('deleted_at', { mode: 'timestamp_ms' }),
  createdAt: integer('created_at', { mode: 'timestamp_ms' }).notNull(),
  updatedAt: integer('updated_at', { mode: 'timestamp_ms' }).notNull(),
});

export const messages = sqliteTable('messages', {
  id: integer('id').primaryKey({ autoIncrement: true }),
  conversationId: integer('conversation_id')
    .notNull()
    .references(() => conversations.id),
  role: text('role', { enum: ['user', 'assistant'] }).notNull(),
  content: text('content').notNull(),
  /** Reasoning text streamed before the answer. Null when the model produced none. */
  reasoning: text('reasoning'),
  /** JSON-encoded array of tool call records, for replaying the turn. */
  toolCalls: text('tool_calls'),
  /**
   * Capability the turn ran in: auto | chat | image | video.
   * Stored on the user message that opened the turn and on its assistant reply,
   * so history can render each turn without pairing rows.
   */
  mode: text('mode'),
  createdAt: integer('created_at', { mode: 'timestamp_ms' }).notNull(),
});

export const assets = sqliteTable('assets', {
  id: integer('id').primaryKey({ autoIncrement: true }),
  messageId: integer('message_id')
    .notNull()
    .references(() => messages.id),
  kind: text('kind', { enum: ['image', 'video'] }).notNull(),
  filePath: text('file_path').notNull(),
  sourceUrl: text('source_url'),
  mime: text('mime').notNull(),
  bytes: integer('bytes').notNull(),
  createdAt: integer('created_at', { mode: 'timestamp_ms' }).notNull(),
});

/**
 * Async provider jobs (video today). A video render takes minutes, which does not
 * fit inside an SSE turn: the tool submits, returns immediately, and a background
 * worker polls the provider, mirrors the bytes at the moment of success (provider
 * URLs expire) and appends the result as a new assistant message.
 */
export const generationTasks = sqliteTable('generation_tasks', {
  id: integer('id').primaryKey({ autoIncrement: true }),
  conversationId: integer('conversation_id')
    .notNull()
    .references(() => conversations.id),
  /** Assistant message of the turn that submitted the task. */
  messageId: integer('message_id'),
  kind: text('kind', { enum: ['image', 'video'] }).notNull(),
  providerTaskId: text('provider_task_id').notNull(),
  model: text('model').notNull(),
  prompt: text('prompt').notNull(),
  /** JSON-encoded provider parameters (size, duration, ...). */
  params: text('params'),
  status: text('status').notNull(),
  attempts: integer('attempts').notNull().default(0),
  error: text('error'),
  assetId: integer('asset_id'),
  /** Assistant message appended when the task finished. */
  resultMessageId: integer('result_message_id'),
  createdAt: integer('created_at', { mode: 'timestamp_ms' }).notNull(),
  updatedAt: integer('updated_at', { mode: 'timestamp_ms' }).notNull(),
  finishedAt: integer('finished_at', { mode: 'timestamp_ms' }),
});

export type User = typeof users.$inferSelect;
export type Conversation = typeof conversations.$inferSelect;
export type Message = typeof messages.$inferSelect;
export type Asset = typeof assets.$inferSelect;
export type GenerationTask = typeof generationTasks.$inferSelect;
