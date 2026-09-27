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
 * What this runner is for: everything that can be decided from the source
 * alone — the pure modules, the services that can be instantiated against a
 * real in-memory schema (scoping, replay, status transitions), and the browser
 * script against jsdom.
 *
 * What it is deliberately NOT for: the agent turn loop
 * (`src/agent/agent.service.ts`) and the provider client
 * (`src/bailian/responses-client.ts`). Their behaviour is defined by a live
 * endpoint — the non-standard SSE comment lines, the reasoning event naming,
 * per-call latency, and what happens when a tool times out mid-stream. A fake
 * provider would only re-encode our assumptions about all of that, and it would
 * keep passing exactly when the provider changed, which is the case worth
 * catching. Dated evidence for that layer lives in docs/verification.md.
 */
export default defineConfig({
  test: {
    projects: [
      {
        // Server and shared modules. Two kinds of test live here: the pure
        // ones (no container, esbuild would have sufficed) and the ones that
        // build a Nest container — hence SWC for the whole project rather than
        // splitting it, which would mean two configs to keep in sync.
        plugins: [decoratorMetadata],
        resolve: { alias },
        test: {
          name: 'server',
          environment: 'node',
          include: ['src/**/*.test.ts'],
        },
      },
      {
        // The browser script. Runs against jsdom; no decorators involved, so it
        // keeps the default esbuild transform.
        resolve: { alias },
        test: {
          name: 'web',
          environment: 'jsdom',
          include: ['web/**/*.test.ts'],
          setupFiles: ['web/scripts/test-setup.ts'],
        },
      },
    ],
  },
});
