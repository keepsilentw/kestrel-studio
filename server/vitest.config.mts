import { resolve } from 'node:path';
import swc from 'unplugin-swc';
import { defineConfig } from 'vitest/config';

const alias = { '@': resolve(import.meta.dirname, 'src') };

/**
 * `emitDecoratorMetadata` is what this plugin exists for. esbuild — vitest's
 * default transform — does not implement it, and the application relies on the
 * `design:paramtypes` metadata it produces for ~19 constructor parameters, so
 * `Test.createTestingModule()` cannot resolve dependencies without it. SWC has
 * a type system, so it emits the metadata properly.
 *
 * This is also why ts-jest was avoided: it needs TypeScript's programmatic
 * compiler API, which TS 7 removed (the same reason the app is pinned to
 * TS 6.0.3). SWC never goes through tsc.
 */
const decoratorMetadata = swc.vite({
  jsc: {
    parser: { syntax: 'typescript', decorators: true },
    transform: { legacyDecorator: true, decoratorMetadata: true },
    target: 'es2022',
  },
  module: { type: 'es6' },
});

/**
 * What this runner is for: everything that can be decided from the source alone —
 * the pure modules, and the services that can be instantiated against a real
 * in-memory schema (scoping, replay, status transitions).
 *
 * What it is deliberately NOT for: the agent turn loop
 * (`server/src/agent/agent.service.ts`) and the provider client
 * (`server/src/bailian/responses-client.ts`). Their behaviour is defined by a live
 * endpoint — the non-standard SSE comment lines, the reasoning event naming,
 * per-call latency, and what happens when a tool times out mid-stream. A fake
 * provider would only re-encode our assumptions about all of that, and it would
 * keep passing exactly when the provider changed, which is the case worth
 * catching. Dated evidence for that layer lives in docs/verification.md.
 *
 * This config belongs to the server app and is discovered by `vitest run` with the
 * working directory at `server/` (`pnpm -C server test`); the browser suite has its
 * own under `web/`. Paths below are relative to this directory, not the repo root.
 */
export default defineConfig({
  plugins: [decoratorMetadata],
  resolve: { alias },
  test: {
    name: 'server',
    environment: 'node',
    include: ['__tests__/**/*.test.ts'],
  },
});
