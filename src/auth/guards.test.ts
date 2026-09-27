import { Controller, Get, Req, Res, UseGuards } from '@nestjs/common';
import { Test } from '@nestjs/testing';
import type { NestExpressApplication } from '@nestjs/platform-express';
import type { NextFunction, Request, Response } from 'express';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { AuthenticatedGuard, ViewAuthGuard } from '@/auth/guards';

/**
 * Exercised over a real request rather than by faking an ExecutionContext —
 * building one that satisfies the interface would need a type assertion, and
 * the observable difference between the two guards (401 vs 302) is the whole
 * point of having two of them.
 *
 * A header decides whether the stub middleware attaches a principal, so the
 * same app serves both the authenticated and anonymous cases.
 */
const AUTH_HEADER = 'x-test-user';

@Controller('json')
class JsonProbeController {
  @UseGuards(AuthenticatedGuard)
  @Get()
  read(@Req() req: Request): { id: number } {
    return { id: req.user?.id ?? -1 };
  }
}

@Controller('html')
class HtmlProbeController {
  @UseGuards(ViewAuthGuard)
  @Get()
  read(@Res() res: Response): void {
    res.status(200).send('shell');
  }
}

let app: NestExpressApplication;
let origin: string;

beforeAll(async () => {
  const moduleRef = await Test.createTestingModule({
    controllers: [JsonProbeController, HtmlProbeController],
  }).compile();

  app = moduleRef.createNestApplication<NestExpressApplication>();
  app.use((req: Request, _res: Response, next: NextFunction): void => {
    if (req.headers[AUTH_HEADER] === 'yes') {
      req.user = { id: 7, username: 'tester', role: 'user' };
    }
    next();
  });

  await app.listen(0);
  origin = await app.getUrl();
});

afterAll(async () => {
  await app.close();
});

function get(path: string, authenticated: boolean): Promise<globalThis.Response> {
  return fetch(`${origin}${path}`, {
    headers: authenticated ? { [AUTH_HEADER]: 'yes' } : {},
    redirect: 'manual',
  });
}

describe('AuthenticatedGuard', () => {
  it('已登录放行', async () => {
    const response = await get('/json', true);
    expect(response.status).toBe(200);
    await expect(response.json()).resolves.toEqual({ id: 7 });
  });

  it('未登录返回 401 及中文提示', async () => {
    const response = await get('/json', false);
    expect(response.status).toBe(401);
    // Asserted loosely on purpose: UnauthorizedException expands into
    // { statusCode, message, error }, whereas the 401s written by hand in
    // chat.controller.ts are `{ message: '未登录' }` alone. The same status
    // therefore has two body shapes across the API; only `message` is common
    // to both, so only `message` is pinned here.
    await expect(response.json()).resolves.toMatchObject({ message: '未登录' });
  });
});

describe('ViewAuthGuard', () => {
  it('已登录渲染页面', async () => {
    const response = await get('/html', true);
    expect(response.status).toBe(200);
    await expect(response.text()).resolves.toBe('shell');
  });

  it('未登录 302 到登录页，而不是 401', async () => {
    // The distinction the two guards exist for: a browser hitting an HTML route
    // should be redirected, a fetch hitting a JSON route should get a status.
    const response = await get('/html', false);
    expect(response.status).toBe(302);
    expect(response.headers.get('location')).toBe('/login');
  });
});
