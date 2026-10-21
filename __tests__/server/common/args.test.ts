import { describe, expect, it } from 'vitest';
import { clamp, readNumber, readOptionalNumber, readOptionalString, readString } from '@/common/args';

/**
 * These guard every value the model puts in a tool call, so the tests pin the
 * coercions that a model can plausibly get wrong: wrong type, blank strings,
 * zero, negative, fractional and non-finite numbers.
 */
describe('readString', () => {
  it('非字符串回退', () => {
    expect(readString({ p: 42 }, 'p', 'fallback')).toBe('fallback');
    expect(readString({ p: null }, 'p', 'fallback')).toBe('fallback');
    expect(readString({}, 'p', 'fallback')).toBe('fallback');
  });

  it('只有空白的字符串视为缺失', () => {
    expect(readString({ p: '   ' }, 'p', 'fallback')).toBe('fallback');
    expect(readString({ p: '' }, 'p', 'fallback')).toBe('fallback');
  });

  it('去掉两端空白', () => {
    expect(readString({ p: '  一只红隼  ' }, 'p', 'fallback')).toBe('一只红隼');
  });
});

describe('readOptionalString', () => {
  it('缺失时返回 null 而非回退值', () => {
    expect(readOptionalString({}, 'p')).toBeNull();
    expect(readOptionalString({ p: '  ' }, 'p')).toBeNull();
  });

  it('有值时去空白', () => {
    expect(readOptionalString({ p: ' x ' }, 'p')).toBe('x');
  });
});

describe('readNumber', () => {
  it('非数值回退', () => {
    expect(readNumber({ n: '3' }, 'n', 1)).toBe(1);
    expect(readNumber({}, 'n', 1)).toBe(1);
  });

  it('0 与负数回退 —— 该读取器要求严格大于 0', () => {
    expect(readNumber({ n: 0 }, 'n', 1)).toBe(1);
    expect(readNumber({ n: -2 }, 'n', 1)).toBe(1);
  });

  it('非有限值回退', () => {
    expect(readNumber({ n: Number.POSITIVE_INFINITY }, 'n', 1)).toBe(1);
    expect(readNumber({ n: Number.NaN }, 'n', 1)).toBe(1);
  });

  it('正数原样返回，不取整', () => {
    expect(readNumber({ n: 2.5 }, 'n', 1)).toBe(2.5);
  });
});

describe('readOptionalNumber', () => {
  it('缺失时返回 null', () => {
    expect(readOptionalNumber({}, 'n')).toBeNull();
    expect(readOptionalNumber({ n: 'x' }, 'n')).toBeNull();
  });

  it('接受 0 与负数 —— 与 readNumber 不同，这里只要求是有限数', () => {
    expect(readOptionalNumber({ n: 0 }, 'n')).toBe(0);
    expect(readOptionalNumber({ n: -2 }, 'n')).toBe(-2);
  });

  it('非有限值返回 null', () => {
    expect(readOptionalNumber({ n: Number.NaN }, 'n')).toBeNull();
    expect(readOptionalNumber({ n: Number.POSITIVE_INFINITY }, 'n')).toBeNull();
  });

  it('向零取整', () => {
    expect(readOptionalNumber({ n: 3.7 }, 'n')).toBe(3);
  });
});

describe('clamp', () => {
  it('先取整再夹取', () => {
    expect(clamp(9.9, 1, 4)).toBe(4);
    expect(clamp(0.5, 1, 4)).toBe(1);
    expect(clamp(3.9, 1, 4)).toBe(3);
  });

  it('区间内的整数原样返回', () => {
    expect(clamp(2, 1, 4)).toBe(2);
  });

  it('负数与 0 被抬到下限', () => {
    expect(clamp(-5, 1, 4)).toBe(1);
    expect(clamp(0, 1, 4)).toBe(1);
  });
});
