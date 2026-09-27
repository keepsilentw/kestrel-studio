import { existsSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it } from 'vitest';
import { resolveAssetPath, storedAssetName } from '@/media/asset-path';

/**
 * The contract between the `assets.file_path` column and the storage directory.
 * Its failure mode is quiet — assets 404 — and it already happened once when the
 * project moved, so both the new form and the legacy absolute one are pinned.
 */
let filesDir: string;
let previousStorageDir: string | undefined;

beforeAll(() => {
  filesDir = mkdtempSync(join(tmpdir(), 'kestrel-asset-path-'));
});

afterAll(() => {
  rmSync(filesDir, { recursive: true, force: true });
});

beforeEach(() => {
  previousStorageDir = process.env.STORAGE_DIR;
  process.env.STORAGE_DIR = filesDir;
});

afterEach(() => {
  if (previousStorageDir === undefined) {
    delete process.env.STORAGE_DIR;
  } else {
    process.env.STORAGE_DIR = previousStorageDir;
  }
  previousStorageDir = undefined;
});

describe('storedAssetName', () => {
  it('只留文件名，不带目录', () => {
    expect(storedAssetName('/app/storage/image-1.png')).toBe('image-1.png');
    expect(storedAssetName(join(filesDir, 'video-2.mp4'))).toBe('video-2.mp4');
  });

  it('拼出来就能在当前目录下找到同名文件', () => {
    expect(join(filesDir, storedAssetName('/whatever/dir/a.png'))).toBe(join(filesDir, 'a.png'));
  });
});

describe('resolveAssetPath', () => {
  it('文件名按当前 STORAGE_DIR 展开', () => {
    expect(resolveAssetPath('image-1.png')).toBe(join(filesDir, 'image-1.png'));
  });

  it('STORAGE_DIR 是相对路径时先 resolve', () => {
    process.env.STORAGE_DIR = 'relative-storage';
    expect(resolveAssetPath('a.png')).toBe(join(resolve('relative-storage'), 'a.png'));
  });

  it('只取文件名，带斜杠的相对值不构成目录穿越', () => {
    expect(resolveAssetPath('../../etc/passwd')).toBe(join(filesDir, 'passwd'));
    expect(resolveAssetPath('nested/a.png')).toBe(join(filesDir, 'a.png'));
  });

  it('搬迁前的绝对路径：文件还在原处就原样沿用', () => {
    const kept = join(filesDir, 'kept.png');
    writeFileSync(kept, 'bytes');

    expect(resolveAssetPath(kept)).toBe(kept);
  });

  it('搬迁前的绝对路径：原处已无此文件时按文件名在当前目录找', () => {
    // The regression: the row pointed into playground/kestrel-studio/storage.
    const moved = join(filesDir, 'moved.png');
    writeFileSync(moved, 'bytes');

    expect(resolveAssetPath(join(tmpdir(), 'kestrel-nowhere', 'moved.png'))).toBe(moved);
    expect(existsSync(moved)).toBe(true);
  });
});
