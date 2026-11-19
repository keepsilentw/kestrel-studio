import type { NextFunction, Request, Response } from 'express';

/**
 * Mirrors `rust/server/src/http/security_headers.rs`.
 *
 * `script-src` keeps `'unsafe-inline'` because the WASM loader is emitted inline
 * in the Rust page shell; the rest of the policy still rules out external
 * scripts, external connections, framing, plugins and foreign form targets.
 */
export const CONTENT_SECURITY_POLICY =
  "default-src 'self'; " +
  "base-uri 'self'; " +
  "form-action 'self'; " +
  "frame-ancestors 'none'; " +
  "object-src 'none'; " +
  "img-src 'self' data: blob:; " +
  "media-src 'self' blob:; " +
  "connect-src 'self'; " +
  "script-src 'self' 'unsafe-inline'; " +
  "style-src 'self' 'unsafe-inline'";

export function securityHeaders(): (
  request: Request,
  response: Response,
  next: NextFunction,
) => void {
  return (_request, response, next) => {
    response.setHeader('X-Content-Type-Options', 'nosniff');
    response.setHeader('X-Frame-Options', 'DENY');
    response.setHeader('Referrer-Policy', 'no-referrer');
    response.setHeader(
      'Permissions-Policy',
      'camera=(), geolocation=(), microphone=(self)',
    );
    response.setHeader('Content-Security-Policy', CONTENT_SECURITY_POLICY);
    next();
  };
}
