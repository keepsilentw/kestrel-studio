import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { loadEnvFile } from '@/config/env-file';

/**
 * This module decides what a developer's local `.env` is allowed to change, so
 * the two properties that matter are: it never overwrites a real environment
 * variable (that is how the container keeps its own values), and a value is
 * taken literally (a password is not a place for interpretation).
 */
const KEYS = ['ENVFIX_ONE', 'ENVFIX_TWO', 'ENVFIX_EMPTY', 'ENVFIX_TRicky'];

let directory: string;

function writeEnv(content: string): void {
  writeFileSync(join(directory, '.env'), content, 'utf8');
}

beforeEach(() => {
  directory = mkdtempSync(join(tmpdir(), 'env-file-'));
});

afterEach(() => {
  rmSync(directory, { recursive: true, force: true });
  for (const key of KEYS) {
    delete process.env[key];
  }
});

describe('loadEnvFile', () => {
  it('没有 .env 时安静返回，不抛错', () => {
    const result = loadEnvFile(directory);
    expect(result.present).toBe(false);
    expect(result.applied).toBe(0);
    expect(result.path).toBe(join(directory, '.env'));
  });

  it('写入键值，跳过注释与空行，剥掉包裹的引号', () => {
    writeEnv(
      [
        '# a comment',
        '',
        'ENVFIX_ONE=plain',
        'ENVFIX_TWO="double quoted"',
        "  ENVFIX_TRicky = 'single'  ",
        '   ',
      ].join('\n'),
    );

    const result = loadEnvFile(directory);
    expect(result).toMatchObject({ present: true, applied: 3, skipped: 0 });
    expect(process.env.ENVFIX_ONE).toBe('plain');
    expect(process.env.ENVFIX_TWO).toBe('double quoted');
    expect(process.env.ENVFIX_TRicky).toBe('single');
  });

  it('值按字面取：等号之后的内容全部保留，包括 # 与空格', () => {
    // A password containing a hash must not be truncated into a weaker one.
    writeEnv('ENVFIX_ONE=a#b=c  \n');
    loadEnvFile(directory);
    expect(process.env.ENVFIX_ONE).toBe('a#b=c');
  });

  it('真实环境优先，已存在的变量不被覆盖', () => {
    // This is the rule that keeps the container's env_file authoritative and
    // lets `PORT=3000 make dev` win over a stale local file.
    process.env.ENVFIX_ONE = 'from-the-shell';
    writeEnv('ENVFIX_ONE=from-the-file\nENVFIX_TWO=only-here\n');

    const result = loadEnvFile(directory);
    expect(result.applied).toBe(1);
    expect(result.skipped).toBe(1);
    expect(process.env.ENVFIX_ONE).toBe('from-the-shell');
    expect(process.env.ENVFIX_TWO).toBe('only-here');
  });

  it('空值与没有键名的行都被处理掉', () => {
    writeEnv(['ENVFIX_EMPTY=', '=no-key', 'NOEQUALS'].join('\r\n'));

    const result = loadEnvFile(directory);
    expect(result.present).toBe(true);
    expect(process.env.ENVFIX_EMPTY).toBe('');
    expect(result.applied).toBe(1);
    expect(result.skipped).toBe(2);
  });
});
