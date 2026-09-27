import { createHmac, timingSafeEqual } from 'node:crypto';
import { loadConfig } from '@/config/configuration';

/**
 * Signed, expiring, session-free URL for one asset.
 *
 * Image-to-video requires handing the provider a URL it can fetch on its own,
 * and the normal download endpoint is behind the session guard. Rather than
 * exposing assets, this signs a single asset id with an expiry so the link is
 * useless once it lapses and cannot be edited into a different id.
 */
const FRAME_TTL_MS = 2 * 60 * 60 * 1000;

function signature(assetId: number, expiresAt: number, secret: string): string {
  return createHmac('sha256', secret).update(`${assetId}.${expiresAt}`).digest('hex');
}

export function buildSignedFrameUrl(assetId: number, now: number = Date.now()): string {
  const { publicBaseUrl, sessionSecret } = loadConfig();
  const expiresAt = now + FRAME_TTL_MS;
  const sig = signature(assetId, expiresAt, sessionSecret);
  return `${publicBaseUrl}/api/assets/${assetId}/frame?exp=${expiresAt}&sig=${sig}`;
}

export function verifySignedFrame(
  assetId: number,
  expiresAt: number,
  provided: string,
): boolean {
  if (!Number.isFinite(expiresAt) || expiresAt < Date.now()) {
    return false;
  }
  const expected = signature(assetId, expiresAt, loadConfig().sessionSecret);
  const a = Buffer.from(expected, 'utf8');
  const b = Buffer.from(provided, 'utf8');
  return a.length === b.length && timingSafeEqual(a, b);
}

/**
 * The provider fetches the frame itself, so a base URL it cannot reach makes
 * image-to-video impossible. Detected up front to give a real reason instead of
 * a provider-side download error minutes later.
 */
export function isPubliclyReachable(): boolean {
  const { publicBaseUrl } = loadConfig();
  try {
    const host = new URL(publicBaseUrl).hostname;
    return host !== 'localhost' && host !== '127.0.0.1' && host !== '::1';
  } catch {
    return false;
  }
}
