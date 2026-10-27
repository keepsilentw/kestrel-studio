// Regenerates rust/src/auth/blowfish_tables.rs from bcryptjs's own tables.
//
// The Blowfish P-array and S-boxes are the hex digits of pi: 1042 constants that
// must not be transcribed by hand. bcryptjs (Apache-2.0) is already a dependency
// of this project, so its copy is the source of truth here.
//
//   node scripts/gen-blowfish-tables.mjs > rust/src/auth/blowfish_tables.rs
import { readFileSync } from 'node:fs';

const SRC = 'node_modules/bcryptjs/index.js';

function grab(name, expected) {
  const source = readFileSync(SRC, 'utf8');
  const start = source.indexOf(`var ${name} = [`);
  if (start < 0) throw new Error(`missing ${name} in ${SRC}`);
  const body = source.slice(source.indexOf('[', start));
  const text = body.slice(1, body.indexOf(']'));
  const numbers = text
    .split(',')
    .map((token) => token.trim())
    .filter(Boolean)
    .map((token) => {
      if (!/^0x[0-9a-f]{1,8}$/i.test(token)) {
        throw new Error(`unexpected token in ${name}: ${token}`);
      }
      return Number.parseInt(token, 16) >>> 0;
    });
  if (numbers.length !== expected) {
    throw new Error(`${name}: got ${numbers.length} entries, want ${expected}`);
  }
  return numbers;
}

const format = (numbers, perLine) => {
  const lines = [];
  for (let i = 0; i < numbers.length; i += perLine) {
    const chunk = numbers
      .slice(i, i + perLine)
      .map((value) => `0x${value.toString(16).padStart(8, '0')}`)
      .join(', ');
    lines.push(`    ${chunk},`);
  }
  return lines.join('\n');
};

const table = (name, length, rows, perLine, attribute = '') =>
  `${attribute}pub const ${name}: [u32; ${length}] = [\n${format(rows, perLine)}\n];`;

const p = grab('P_ORIG', 18);
const s = grab('S_ORIG', 1024);
const c = grab('C_ORIG', 6);

console.log(
  [
    '// Generated from bcryptjs 3.0.3 (Apache-2.0) - the standard Blowfish P-array and',
    '// S-boxes (the hex digits of pi) plus the bcrypt magic constant "Orphean-',
    '// BeholderScryDoubt". Do not hand-edit; regenerate with:',
    '//   node scripts/gen-blowfish-tables.mjs > rust/src/auth/blowfish_tables.rs',
    '',
    table('P_ORIG', 18, p, 6, '#[allow(clippy::upper_case_acronyms)]\n'),
    '',
    table('S_ORIG', 1024, s, 8),
    '',
    table('C_ORIG', 6, c, 6),
  ].join('\n'),
);
