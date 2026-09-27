/** Helpers for reading loosely-typed tool arguments coming from the model. */

/**
 * Parses the JSON blob a model sent as function-call arguments.
 *
 * Degrades to `{}` rather than throwing: the model is free to send `""`,
 * truncated JSON or a bare array, and the tool's own readers then supply their
 * fallbacks.
 */
export function parseToolArguments(raw: string): Record<string, unknown> {
  if (raw.trim().length === 0) {
    return {};
  }
  try {
    return asRecord(JSON.parse(raw));
  } catch {
    return {};
  }
}

/**
 * Narrows an unknown value to a plain record without an assertion. Used for tool
 * arguments and for form bodies, which arrive with the same "anything at all"
 * type and get read with the same `readX` helpers.
 */
export function asRecord(value: unknown): Record<string, unknown> {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) {
    return {};
  }
  return toRecord(value);
}

/** Narrows an already-parsed object without an assertion. */
function toRecord(source: object): Record<string, unknown> {
  const result: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(source)) {
    result[key] = value;
  }
  return result;
}

export function readString(
  args: Record<string, unknown>,
  key: string,
  fallback: string,
): string {
  const value = args[key];
  return typeof value === 'string' && value.trim().length > 0 ? value.trim() : fallback;
}

export function readOptionalString(args: Record<string, unknown>, key: string): string | null {
  const value = args[key];
  return typeof value === 'string' && value.trim().length > 0 ? value.trim() : null;
}

export function readNumber(
  args: Record<string, unknown>,
  key: string,
  fallback: number,
): number {
  const value = args[key];
  return typeof value === 'number' && Number.isFinite(value) && value > 0 ? value : fallback;
}

export function readOptionalNumber(args: Record<string, unknown>, key: string): number | null {
  const value = args[key];
  return typeof value === 'number' && Number.isFinite(value) ? Math.trunc(value) : null;
}

export function clamp(value: number, min: number, max: number): number {
  return Math.min(Math.max(Math.trunc(value), min), max);
}
