import { type Config, defineConfig } from 'drizzle-kit';

// Paths below are resolved against the **working directory**, and `pnpm db:push`
// runs from the repo root (`drizzle-kit push --config nest/server/drizzle.config.ts`) —
// drizzle-kit globs `schema` relative to cwd, not to this file. Keeping the cwd at
// the root is also what makes the default `url` point at the same SQLite file both
// servers open, instead of a fresh `nest/server/data/`.
const config: Config = defineConfig({
  dialect: 'sqlite',
  schema: './nest/server/src/database/schema.ts',
  out: './nest/server/drizzle',
  dbCredentials: {
    url: process.env.DATABASE_FILE ?? 'data/kestrel-studio.db',
  },
});

export default config;
