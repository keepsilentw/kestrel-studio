import path from 'node:path';
import { register } from 'tsconfig-paths';

/**
 * Makes the `@/*` alias resolvable at runtime.
 *
 * TypeScript only rewrites aliases for type checking, never for emitted code,
 * so the compiled `require('@/...')` calls need a resolver. tsconfig-paths
 * cannot do it by reading tsconfig.json here: TypeScript 7 removed `baseUrl`,
 * and tsconfig-paths resolves every path entry against it, throwing
 * ERR_INVALID_ARG_TYPE when it is absent. Passing baseUrl explicitly sidesteps
 * that; after compilation __dirname is dist/, which is the root the aliases
 * point into.
 *
 * Must stay the first import of the entry point: aliases used by any module
 * loaded earlier would be resolved before this hook is installed.
 */
register({
  baseUrl: __dirname,
  paths: { '@/*': ['*'] },
});
