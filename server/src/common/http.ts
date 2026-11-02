/** Shared parsing for route/query parameters, which arrive as strings. */

/** Returns the value as a positive integer, or null when it is not one. */
export function readPositiveInt(value: unknown): number | null {
  const parsed = Number(value);
  return Number.isInteger(parsed) && parsed > 0 ? parsed : null;
}

/**
 * Returns the value as a non-negative integer, or null when it is not one.
 * Stream cursors use this: 0 is a legal "from the beginning" position, while
 * an id parsed out of a header or query must still be a whole number.
 */
export function readNonNegativeInt(value: unknown): number | null {
  const parsed = Number(value);
  return Number.isInteger(parsed) && parsed >= 0 ? parsed : null;
}

/** Returns the value when it is a string, the empty string otherwise. */
export function readString(source: unknown, key: string): string {
  if (typeof source !== 'object' || source === null) {
    return '';
  }
  const value = (source as Record<string, unknown>)[key];
  return typeof value === 'string' ? value : '';
}
