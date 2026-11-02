import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

/**
 * Where the `.hbs` templates live **in the source tree**.
 *
 * `nest/server/src/main.ts` derives its own view directory from `__dirname`, which after
 * compilation is `nest/server/dist/view` — same relative shape, one tree deeper. Tests run
 * against the sources, so they need the source location, and it is anchored to *this*
 * module rather than to the process working directory. That anchoring is not styling:
 * the moment the backend moved out of the repo root, a `process.cwd()`-based path kept
 * resolving to a directory that no longer exists and every render in the suite failed
 * with `Failed to lookup view` — an error that only surfaces once the runner is allowed
 * to bind a socket, because the whole file dies at bootstrap otherwise.
 */
export const VIEWS_DIR = join(fileURLToPath(import.meta.url), '..', '..', 'src', 'view', 'views');
