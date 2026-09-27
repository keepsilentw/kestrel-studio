import { Controller, Get, Req, UseGuards } from '@nestjs/common';
import { Test } from '@nestjs/testing';
import type { NestExpressApplication } from '@nestjs/platform-express';
import { PassportModule } from '@nestjs/passport';
import { hash } from 'bcryptjs';
import Database from 'better-sqlite3';
import { drizzle } from 'drizzle-orm/better-sqlite3';
import connectSqliteStore from 'better-sqlite3-session-store';
import type { Request } from 'express';
import session from 'express-session';
import passport from 'passport';
import { afterAll, beforeAll, beforeEach, describe, expect, it } from 'vitest';
import { AuthController } from '@/auth/auth.controller';
import { AuthService } from '@/auth/auth.service';
import { AuthenticatedGuard } from '@/auth/guards';
import { LocalStrategy } from '@/auth/local.strategy';
import { SessionSerializer } from '@/auth/session.serializer';
import { applySchema, DRIZZLE_INSTANCE } from '@/database/database.module';
import * as schema from '@/database/schema';

/**
 * The whole login path over real HTTP, with the session and passport middleware
 * wired as src/main.ts wires them.
 *
 * This is the test that would have caught the incident recorded in auth.controller.ts:
 * @nestjs/passport's AuthGuard only authenticates, it never writes the session,
 * so a login can answer 302 and still leave the caller anonymous on the next
 * request. Asserting the redirect alone would not notice — hence the probe
 * route below, which is fetched with the cookie the login handed back.
 */
const PASSWORD = 'correct-horse-battery';
const COST = 4;

@Controller('probe')
class ProbeController {
  @UseGuards(AuthenticatedGuard)
  @Get()
  read(@Req() req: Request): { username: string; id: number } {
    return { username: req.user?.username ?? '', id: req.user?.id ?? -1 };
  }
}

let app: NestExpressApplication;
let origin: string;
let connection: Database.Database;
let aliceId: number;

async function bootstrap(): Promise<void> {
  connection = new Database(':memory:');
  applySchema(connection);
  const db = drizzle(connection, { schema });

  const created = db
    .insert(schema.users)
    .values({
      username: 'alice',
      passwordHash: await hash(PASSWORD, COST),
      role: 'user',
      createdAt: new Date(),
    })
    .run();
  aliceId = Number(created.lastInsertRowid);

  const moduleRef = await Test.createTestingModule({
    // Mirrors AuthModule: without PassportModule, AuthGuard has no
    // AuthModuleOptions to resolve.
    imports: [PassportModule.register({ session: true })],
    controllers: [AuthController, ProbeController],
    providers: [
      AuthService,
      LocalStrategy,
      SessionSerializer,
      { provide: DRIZZLE_INSTANCE, useValue: db },
    ],
  }).compile();

  app = moduleRef.createNestApplication<NestExpressApplication>();

  const SessionStore = connectSqliteStore(session);
  app.use(
    session({
      store: new SessionStore({
        client: connection,
        expired: { clear: true, intervalMs: 15 * 60 * 1000 },
      }),
      secret: 'test-secret',
      resave: false,
      saveUninitialized: false,
      cookie: { httpOnly: true, sameSite: 'lax' },
    }),
  );
  app.use(passport.initialize());
  app.use(passport.session());

  await app.listen(0);
  origin = await app.getUrl();
}

/** fetch has no cookie jar, so the session cookie is carried by hand. */
function cookieOf(response: globalThis.Response): string {
  const header = response.headers.get('set-cookie');
  if (header === null) {
    return '';
  }
  return header.split(';')[0];
}

function login(username: unknown, password: unknown): Promise<globalThis.Response> {
  return fetch(`${origin}/login`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ username, password }),
    redirect: 'manual',
  });
}

function probe(cookie: string): Promise<globalThis.Response> {
  return fetch(`${origin}/probe`, {
    headers: cookie.length > 0 ? { cookie } : {},
    redirect: 'manual',
  });
}

beforeAll(async () => {
  await bootstrap();
});

afterAll(async () => {
  await app.close();
  connection.close();
});

beforeEach(() => {
  // Sessions live in their own table, managed by the store; clearing the
  // login state keeps each test starting anonymous.
  connection.exec('DELETE FROM sessions');
});

describe('POST /login — 成功', () => {
  it('凭据正确时 302 到首页并下发会话 cookie', async () => {
    const response = await login('alice', PASSWORD);
    expect(response.status).toBe(302);
    expect(response.headers.get('location')).toBe('/');
    expect(cookieOf(response)).toContain('connect.sid');
  });

  it('会话真的写进去了 —— 带上 cookie 后受保护路由放行', async () => {
    const cookie = cookieOf(await login('alice', PASSWORD));

    const response = await probe(cookie);
    expect(response.status).toBe(200);
    await expect(response.json()).resolves.toEqual({ username: 'alice', id: aliceId });
  });

  it('cookie 是 httpOnly', async () => {
    const response = await login('alice', PASSWORD);
    expect(response.headers.get('set-cookie')).toContain('HttpOnly');
  });

  it('不带 cookie 时受保护路由仍是 401', async () => {
    await login('alice', PASSWORD);
    expect((await probe('')).status).toBe(401);
  });
});

describe('POST /login — 失败', () => {
  it('密码错误时回到登录页并带错误标记', async () => {
    const response = await login('alice', 'wrong-password');
    expect(response.status).toBe(302);
    expect(response.headers.get('location')).toBe('/login?error=1');
  });

  it('用户不存在时同样回到登录页', async () => {
    const response = await login('nobody', PASSWORD);
    expect(response.headers.get('location')).toBe('/login?error=1');
  });

  it('缺少字段时不抛 500，而是回到登录页', async () => {
    const response = await login(undefined, undefined);
    expect(response.status).toBe(302);
    expect(response.headers.get('location')).toBe('/login?error=1');
  });

  it('失败不留会话 cookie', async () => {
    const response = await login('alice', 'wrong-password');
    expect(cookieOf(response)).toBe('');
  });

  it('失败后受保护路由仍然 401', async () => {
    const cookie = cookieOf(await login('alice', 'wrong-password'));
    expect((await probe(cookie)).status).toBe(401);
  });
});

describe('POST /logout', () => {
  it('退出后 302 回登录页，且原 cookie 失效', async () => {
    const cookie = cookieOf(await login('alice', PASSWORD));
    expect((await probe(cookie)).status).toBe(200);

    const response = await fetch(`${origin}/logout`, {
      method: 'POST',
      headers: { cookie },
      redirect: 'manual',
    });
    expect(response.status).toBe(302);
    expect(response.headers.get('location')).toBe('/login');

    // The session is destroyed server-side, so replaying the same cookie must
    // not restore the principal.
    expect((await probe(cookie)).status).toBe(401);
  });

  it('未登录时退出也不报错', async () => {
    const response = await fetch(`${origin}/logout`, { method: 'POST', redirect: 'manual' });
    expect(response.status).toBe(302);
  });
});
