import { Injectable } from '@nestjs/common';

/**
 * Mirrors `rust/server/src/auth/login_limit.rs`: five failures per account and
 * address, twenty per address, in a fifteen-minute fixed window. Both stacks run
 * one process behind the same public entry point, so a process-local map is the
 * right scope and a restart intentionally clears the counters.
 */
export const MAX_IDENTITY_FAILURES = 5;
export const MAX_IP_FAILURES = 20;
export const LOGIN_WINDOW_MS = 15 * 60 * 1000;

interface Bucket {
  failures: number;
  startedAt: number;
}

@Injectable()
export class LoginLimiter {
  private readonly entries = new Map<string, Bucket>();

  /** Seconds until this identity/address may try again, or null while allowed. */
  blockedFor(ip: string, username: string): number | null {
    const now = Date.now();
    this.prune(now);
    let wait: number | null = null;
    const checks: Array<readonly [string, number]> = [
      [identityKey(ip, username), MAX_IDENTITY_FAILURES],
      [ipKey(ip), MAX_IP_FAILURES],
    ];
    for (const [key, maximum] of checks) {
      const bucket = this.entries.get(key);
      if (bucket === undefined || bucket.failures < maximum) {
        continue;
      }
      const remaining = Math.max(
        1,
        Math.ceil((LOGIN_WINDOW_MS - (now - bucket.startedAt)) / 1000),
      );
      wait = wait === null ? remaining : Math.max(wait, remaining);
    }
    return wait;
  }

  recordFailure(ip: string, username: string): void {
    const now = Date.now();
    this.prune(now);
    for (const key of [identityKey(ip, username), ipKey(ip)]) {
      const bucket = this.entries.get(key);
      if (bucket === undefined) {
        this.entries.set(key, { failures: 1, startedAt: now });
      } else {
        bucket.failures += 1;
      }
    }
  }

  /** A correct password clears the account bucket, not the address bucket. */
  recordSuccess(ip: string, username: string): void {
    this.entries.delete(identityKey(ip, username));
  }

  private prune(now: number): void {
    for (const [key, bucket] of this.entries) {
      if (now - bucket.startedAt >= LOGIN_WINDOW_MS) {
        this.entries.delete(key);
      }
    }
  }
}

function normalizeIp(ip: string): string {
  return ip.trim().slice(0, 64);
}

function normalizeUsername(username: string): string {
  return username.trim().toLowerCase().slice(0, 64);
}

function identityKey(ip: string, username: string): string {
  return `id:${normalizeIp(ip)}|${normalizeUsername(username)}`;
}

function ipKey(ip: string): string {
  return `ip:${normalizeIp(ip)}`;
}
