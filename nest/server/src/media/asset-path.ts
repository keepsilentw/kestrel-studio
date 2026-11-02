import { existsSync } from 'node:fs';
import { unlink } from 'node:fs/promises';
import { basename, isAbsolute, join, resolve, sep } from 'node:path';
import { loadConfig } from '@/config/configuration';

/**
 * What goes into `assets.file_path`, and how it comes back out.
 *
 * The column holds the file **name**, not a path. An absolute path baked in at
 * write time stops being true the moment the project moves or the storage
 * directory changes, and this bit once already: every asset written before the
 * project left `playground/` kept pointing at the old directory, so the download
 * endpoint answered 404 for the owner too. Where the bytes live is a property of
 * the running instance, so it belongs at read time, not in the row.
 *
 * Rows written before the change hold an absolute path. One that is still there
 * is honoured as-is, so an existing deployment keeps serving without a migration.
 */

/** The value to persist for a file that has just been written. */
export function storedAssetName(filePath: string): string {
  return basename(filePath);
}

/** The file a stored value points at right now. */
export function resolveAssetPath(stored: string): string {
  if (isAbsolute(stored) && existsSync(stored)) {
    return stored;
  }
  return join(resolve(loadConfig().storageDir), basename(stored));
}

/**
 * Deletes mirrored bytes, best effort: the rows are already gone by the time this
 * runs, and a file that is missing or unreadable is nothing left to reclaim.
 * Anything resolving outside STORAGE_DIR is skipped rather than followed — only
 * rows written before the column held a bare name can point there.
 */
export async function removeAssetFiles(storedNames: string[]): Promise<void> {
  const root = resolve(loadConfig().storageDir);
  for (const stored of storedNames) {
    const filePath = resolveAssetPath(stored);
    if (!filePath.startsWith(root + sep)) {
      continue;
    }
    try {
      await unlink(filePath);
    } catch {
      // Already gone.
    }
  }
}
