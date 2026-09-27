/**
 * The package ships no type definitions. This mirrors its documented surface:
 * a factory taking express-session that returns a Store class.
 */
declare module 'better-sqlite3-session-store' {
  import type { Database } from 'better-sqlite3';
  import type { Store } from 'express-session';

  interface SqliteStoreOptions {
    client: Database;
    expired?: {
      clear?: boolean;
      intervalMs?: number;
    };
  }

  type StoreConstructor = new (options: SqliteStoreOptions) => Store;

  function connectSqliteStore(session: unknown): StoreConstructor;

  export default connectSqliteStore;
}
