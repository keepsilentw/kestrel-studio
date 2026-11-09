import Database from 'better-sqlite3';
import { describe, expect, it } from 'vitest';
import { applySchema } from '@/database/database.module';

/**
 * The schema path both backends run at startup. Two stacks share one database file
 * now (docs/roadmap.md §4.5), so this is not just "does the table exist" — it is the
 * migration that gives the three ownerless writes their owner, and the repair that
 * lets the journal's cursor index become UNIQUE without refusing to boot.
 *
 * `rust/server/src/db.rs`'s own tests pin the same two behaviours against the same
 * DDL, because the two DDL strings are kept in sync by hand.
 */
function freshSchema(): Database.Database {
  const connection = new Database(':memory:');
  applySchema(connection);
  return connection;
}

function cursorIndexIsUnique(connection: Database.Database): boolean {
  const row = connection
    .prepare(
      `SELECT "unique" AS isUnique FROM pragma_index_list('conversation_events') WHERE name = 'idx_events_conversation'`,
    )
    .get() as { isUnique: number } | undefined;
  return row !== undefined && row.isUnique === 1;
}

describe('applySchema — 双后端所需的所有权列', () => {
  it('补上写入者标记与任务租约列，重复执行不再补第二遍', () => {
    const connection = freshSchema();
    applySchema(connection);

    const columnsOf = (table: string): string[] =>
      (
        connection.prepare(`SELECT name FROM pragma_table_info(?)`).all(table) as { name: string }[]
      ).map((row) => row.name);

    expect(columnsOf('messages')).toContain('owner');
    expect(columnsOf('generation_tasks')).toEqual(
      expect.arrayContaining(['claimed_by', 'claimed_at']),
    );
  });

  it('游标索引是 UNIQUE，重复的 seq 落不进库', () => {
    const connection = freshSchema();
    expect(cursorIndexIsUnique(connection)).toBe(true);

    connection
      .prepare(
        `INSERT INTO users (id, username, password_hash, role, created_at) VALUES (1,'u','h','user',1)`,
      )
      .run();
    connection
      .prepare(
        `INSERT INTO conversations (id, user_id, title, created_at, updated_at) VALUES (1,1,'t',1,1)`,
      )
      .run();
    const insert = connection.prepare(
      `INSERT INTO conversation_events (conversation_id, seq, message_id, type, payload, created_at) VALUES (1, ?, NULL, ?, '{}', 1)`,
    );
    insert.run(1, 'text');
    // The exact damage the plain index allowed: two rows, one cursor value.
    expect(() => insert.run(1, 'text')).toThrow(/UNIQUE constraint failed/);
  });
});

describe('applySchema — 重复游标的修复', () => {
  it('把旧库里重复的游标行挪到末尾，然后才立起 UNIQUE', () => {
    const connection = freshSchema();
    // Wind the database back to the pre-migration shape: plain index, duplicates in it.
    connection.exec('DROP INDEX idx_events_conversation');
    connection.exec(
      'CREATE INDEX idx_events_conversation ON conversation_events(conversation_id, seq)',
    );
    connection
      .prepare(
        `INSERT INTO users (id, username, password_hash, role, created_at) VALUES (1,'u','h','user',1)`,
      )
      .run();
    connection
      .prepare(
        `INSERT INTO conversations (id, user_id, title, created_at, updated_at) VALUES (1,1,'t',1,1)`,
      )
      .run();
    const insert = connection.prepare(
      `INSERT INTO conversation_events (conversation_id, seq, message_id, type, payload, created_at) VALUES (1, ?, NULL, ?, '{}', 1)`,
    );
    insert.run(1, 'a');
    insert.run(1, 'b');
    insert.run(2, 'c');
    insert.run(2, 'd');

    applySchema(connection);

    // The lowest rowid of each contested pair keeps the value every cursor has already
    // been served from; the losers — rows no reader could ever have seen, because both
    // live delivery and replay filter `seq >` — move to the tail in rowid order.
    const kept = connection
      .prepare(`SELECT seq, type FROM conversation_events ORDER BY seq ASC`)
      .all() as { seq: number; type: string }[];
    expect(kept).toEqual([
      { seq: 1, type: 'a' },
      { seq: 2, type: 'c' },
      { seq: 3, type: 'b' },
      { seq: 4, type: 'd' },
    ]);
    expect(cursorIndexIsUnique(connection)).toBe(true);
  });
});
