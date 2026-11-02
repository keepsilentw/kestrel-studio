import { resolve } from 'node:path';
import { defineConfig } from 'vite';

/**
 * Builds only the progressive-enhancement layer. Templates come from Nest/hbs,
 * so there is no HTML entry here — just the scripts and stylesheet, emitted with
 * stable names the templates can link to.
 *
 * One entry per page: `assets/main.js` for the chat page, `assets/voice.js` for
 * /voice. Entry names are the rollup input keys, which is why they are spelled
 * out rather than hashed.
 *
 * The config lives inside the client it builds, so `root` is this directory and
 * every path is anchored at `import.meta.dirname` rather than the shell's working
 * directory — `vite build --config nest/client/vite.config.mts` behaves the same
 * from anywhere. The output is the shared `public/` at the repo root: both servers
 * serve that one directory (`nest/server/src/main.ts` through `useStaticAssets`,
 * the Rust binary through `STATIC_DIR`), and it stays outside the stack trees
 * because it is a build product, not a source file.
 */
export default defineConfig({
  root: import.meta.dirname,
  publicDir: resolve(import.meta.dirname, 'public'),
  build: {
    outDir: resolve(import.meta.dirname, '../../public'),
    emptyOutDir: true,
    rollupOptions: {
      input: {
        main: resolve(import.meta.dirname, 'scripts/main.ts'),
        voice: resolve(import.meta.dirname, 'scripts/voice.ts'),
      },
      output: {
        entryFileNames: 'assets/[name].js',
        chunkFileNames: 'assets/[name].js',
        assetFileNames: 'assets/[name][extname]',
      },
    },
  },
});
