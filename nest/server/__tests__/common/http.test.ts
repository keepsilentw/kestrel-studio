import { describe, expect, it } from 'vitest';
import { readPositiveInt } from '@/common/http';

describe('readPositiveInt', () => {
  it('接受正整数', () => {
    expect(readPositiveInt('42')).toBe(42);
    expect(readPositiveInt(42)).toBe(42);
  });

  it('容忍两端空白', () => {
    expect(readPositiveInt(' 7 ')).toBe(7);
  });

  it('接受带小数点的整数值', () => {
    // Number('3.0') is 3, which passes Number.isInteger.
    expect(readPositiveInt('3.0')).toBe(3);
  });

  it('非整数与零以下一律返回 null', () => {
    expect(readPositiveInt('3.5')).toBeNull();
    expect(readPositiveInt('0')).toBeNull();
    expect(readPositiveInt('-1')).toBeNull();
  });

  // Labelled explicitly: `%s` renders null/undefined/[] as blanks or as
  // repeated "undefined", which makes a failure message unreadable.
  const invalid: readonly { label: string; value: unknown }[] = [
    { label: '空字符串', value: '' },
    { label: '非数字字符串', value: 'abc' },
    { label: 'NaN 字符串', value: 'NaN' },
    { label: 'null', value: null },
    { label: 'undefined', value: undefined },
    { label: '空数组', value: [] },
  ];

  it.each(invalid)('$label 返回 null', ({ value }) => {
    expect(readPositiveInt(value)).toBeNull();
  });

  it('Number 的进制与科学计数法解析会一并生效', () => {
    // This parses an id straight out of the URL, so the coercion is worth
    // pinning: /api/conversations/0x10/messages yields id 16, not null.
    // Ownership is checked separately (isOwnedBy), so this is a quirk rather
    // than a hole — but it is not a plain "digits only" parse.
    expect(readPositiveInt('0x10')).toBe(16);
    expect(readPositiveInt('1e3')).toBe(1000);
  });
});
