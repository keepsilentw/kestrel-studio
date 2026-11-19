/** The request fields needed to pick an address for throttling. */
export interface ClientAddressRequest {
  readonly headers: Record<string, string | string[] | undefined>;
  readonly ip?: string;
}

function headerValue(value: string | string[] | undefined): string | null {
  if (Array.isArray(value)) {
    const first = value[0]?.trim();
    return first !== undefined && first.length > 0 ? first : null;
  }
  if (typeof value === 'string' && value.trim().length > 0) {
    return value.trim();
  }
  return null;
}

/**
 * openresty is the only public entry point and sets one of these headers. A
 * direct connection falls back to `req.ip`, then to a single shared bucket, so a
 * missing proxy header cannot silently remove the bound.
 */
export function clientIp(request: ClientAddressRequest): string {
  const real = headerValue(request.headers['x-real-ip']);
  const forwarded = headerValue(request.headers['x-forwarded-for'])
    ?.split(',')[0]
    ?.trim();
  const raw = real ?? forwarded ?? request.ip ?? 'direct';
  const trimmed = raw.trim().slice(0, 64);
  return trimmed.length > 0 ? trimmed : 'direct';
}
