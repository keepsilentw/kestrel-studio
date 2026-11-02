import { type Config, defineConfig } from 'drizzle-kit';

// Paths below are resolved against the **working directory**, and `pnpm db:push`
// runs from the repo root (`drizzle-kit push --config server/drizzle.config.ts`) —
// drizzle-kit globs `schema` relative to cwd, not to this file. Keeping the cwd at
// the root is also what makes the default `url` point at the same SQLite file both
// servers open, instead of a fresh `server/data/`.
const config: Config = defineConfig({
  dialect: 'sqlite',
  schema: './server/src/database/schema.ts',
  out: './server/drizzle',
  dbCredentials: {
    url: process.env.DATABASE_FILE ?? 'data/kestrel-studio.db',
  },
});

export default config;
