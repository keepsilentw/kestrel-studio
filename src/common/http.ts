/** Shared parsing for route/query parameters, which arrive as strings. */

/** Returns the value as a positive integer, or null when it is not one. */
export function readPositiveInt(value: unknown): number | null {
  const parsed = Number(value);
  return Number.isInteger(parsed) && parsed > 0 ? parsed : null;
}
