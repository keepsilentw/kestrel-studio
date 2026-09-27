import './alias-bootstrap';
import { NestFactory } from '@nestjs/core';
import type { NestExpressApplication } from '@nestjs/platform-express';
import Database from 'better-sqlite3';
import connectSqliteStore from 'better-sqlite3-session-store';
import session from 'express-session';
import hbs from 'hbs';
import { join } from 'node:path';
import passport from 'passport';
import { AppModule } from '@/app.module';
import { loadConfig } from '@/config/configuration';
import { SQLITE_CONNECTION } from '@/database/database.module';

async function bootstrap(): Promise<void> {
  const config = loadConfig();
  const app = await NestFactory.create<NestExpressApplication>(AppModule);
  app.disable('x-powered-by');

  app.useStaticAssets(join(__dirname, '..', 'public'), { index: false });
  app.setBaseViewsDir(join(__dirname, 'view', 'views'));
  app.setViewEngine('hbs');
  hbs.registerPartials(join(__dirname, 'view', 'views', 'partials'));
  // Handlebars has no comparison operator; the turn partial needs one to pick
  // between <img> and <video> for an asset.
  hbs.registerHelper('eq', (a: unknown, b: unknown): boolean => a === b);

  // Reuse the application's connection rather than opening a second one.
  const sqlite = app.get<Database.Database>(SQLITE_CONNECTION);
  const SessionStore = connectSqliteStore(session);

  app.use(
    session({
      store: new SessionStore({
        client: sqlite,
        expired: { clear: true, intervalMs: 15 * 60 * 1000 },
      }),
      secret: config.sessionSecret,
      resave: false,
      saveUninitialized: false,
      cookie: {
        httpOnly: true,
        sameSite: 'lax',
        maxAge: 7 * 24 * 60 * 60 * 1000,
      },
    }),
  );

  app.use(passport.initialize());
  app.use(passport.session());

  await app.listen(config.port);
}

bootstrap().catch((error: unknown) => {
  console.error(error);
  process.exit(1);
});
