import { existsSync } from 'node:fs';
import { join, normalize } from 'node:path';
import type { NextFunction, Request, Response } from 'express';

/** A token with `q=0` is a refusal, not an acceptance. */
export function acceptsGzip(value: string | string[] | undefined): boolean {
  const raw = Array.isArray(value) ? value.join(',') : value;
  if (raw === undefined) {
    return false;
  }
  return raw.split(',').some((part) => {
    const pieces = part.split(';');
    const token = pieces[0]?.trim().toLowerCase();
    if (token !== 'gzip') {
      return false;
    }
    return !pieces.some((piece) => {
      const normalized = piece.trim().toLowerCase();
      return normalized === 'q=0' || normalized === 'q=0.0';
    });
  });
}

/**
 * Serves the pre-compressed wasm next to the original URL.
 *
 * openresty already gzips JS/CSS but not `application/wasm`, so `make build-wasm`
 * leaves `kestrel_client_bg.wasm.gz` beside the file. This middleware runs before
 * the static handler and only takes over that one path.
 */
export function wasmGzip(
  publicDir: string,
): (request: Request, response: Response, next: NextFunction) => void {
  return (request, response, next) => {
    if (request.method !== 'GET' && request.method !== 'HEAD') {
      next();
      return;
    }
    if (!request.path.startsWith('/wasm/') || !request.path.endsWith('.wasm')) {
      next();
      return;
    }
    if (!acceptsGzip(request.headers['accept-encoding'])) {
      next();
      return;
    }
    const file = normalize(join(publicDir, request.path));
    if (!file.startsWith(normalize(publicDir)) || !existsSync(`${file}.gz`)) {
      next();
      return;
    }

    response.setHeader('Content-Type', 'application/wasm');
    response.setHeader('Content-Encoding', 'gzip');
    response.setHeader('Vary', 'Accept-Encoding');
    response.setHeader('Cache-Control', 'no-cache');
    response.sendFile(`${file}.gz`, (error?: Error) => {
      if (error !== undefined && !response.headersSent) {
        next();
      }
    });
  };
}
