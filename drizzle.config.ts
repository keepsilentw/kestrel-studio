import { type Config, defineConfig } from 'drizzle-kit';

const config: Config = defineConfig({
  dialect: 'sqlite',
  schema: './src/database/schema.ts',
  out: './drizzle',
  dbCredentials: {
    url: process.env.DATABASE_FILE ?? 'data/kestrel-studio.db',
  },
});

export default config;
