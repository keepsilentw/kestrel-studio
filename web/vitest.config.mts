import { defineConfig } from 'vitest/config';

/**
 * The browser suite: the two page scripts driven against jsdom.
 *
 * No SWC here — nothing in this tree uses decorators, so the default esbuild
 * transform is enough, and there is no `@/` alias because the scripts import each
 * other relatively. The value of this suite is documented in
 * docs/implementation-notes.md §8: it exists because "a clean console" failed to
 * catch a client that opened the socket and never sent the `start` frame. That
 * guarantee is a frame-ordering property, so it survives in Rust host tests
 * (`web/client/src/voice_core.rs`) once the WASM client takes over
 * (docs/roadmap.md §4 P3), at which point this config retires with the TS scripts.
 *
 * Discovered by `vitest run` with the working directory at `web/`
 * (`pnpm -C web test`); paths are relative to this directory.
 */
export default defineConfig({
  test: {
    name: 'web',
    environment: 'jsdom',
    include: ['__tests__/**/*.test.ts'],
    setupFiles: ['__tests__/test-setup.ts'],
  },
});
