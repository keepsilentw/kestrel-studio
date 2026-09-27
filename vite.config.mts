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
 */
export default defineConfig({
  root: resolve(import.meta.dirname, 'web'),
  publicDir: resolve(import.meta.dirname, 'web/public'),
  build: {
    outDir: resolve(import.meta.dirname, 'public'),
    emptyOutDir: true,
    rollupOptions: {
      input: {
        main: resolve(import.meta.dirname, 'web/scripts/main.ts'),
        voice: resolve(import.meta.dirname, 'web/scripts/voice.ts'),
      },
      output: {
        entryFileNames: 'assets/[name].js',
        chunkFileNames: 'assets/[name].js',
        assetFileNames: 'assets/[name][extname]',
      },
    },
  },
});
