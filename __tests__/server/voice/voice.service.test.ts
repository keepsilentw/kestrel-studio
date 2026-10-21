import { Test } from '@nestjs/testing';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ToolRegistry } from '@/agent/tools';
import { ConversationService } from '@/conversation/conversation.service';
import { RealtimeClient } from '@/voice/realtime-client';
import { VoiceService } from '@/voice/voice.service';

const TICKET_TTL_MS = 60_000;

/**
 * Only the ticket store is covered here. Everything past it is a relay to a live
 * endpoint, which a fake would misrepresent — that layer is exercised by
 * scripts and recorded in docs/verification.md §3, per the boundary in
 * vitest.config.mts.
 *
 * The ticket store is worth covering because it is the whole authorization of
 * the socket: an upgrade never reaches Express, so nothing else guards it.
 */
interface Harness {
  voice: VoiceService;
}

let harness: Harness;

async function buildHarness(): Promise<Harness> {
  const moduleRef = await Test.createTestingModule({
    providers: [
      VoiceService,
      { provide: RealtimeClient, useValue: {} },
      { provide: ToolRegistry, useValue: {} },
      { provide: ConversationService, useValue: {} },
    ],
  }).compile();
  return { voice: moduleRef.get(VoiceService) };
}

beforeEach(async () => {
  harness = await buildHarness();
});

afterEach(() => {
  vi.useRealTimers();
});

describe('VoiceService 票据', () => {
  it('签发的票据能换回签发给它的用户', () => {
    const { ticket } = harness.voice.issueTicket(7);
    expect(harness.voice.consumeTicket(ticket)).toBe(7);
  });

  it('票据是一次性的', () => {
    const { ticket } = harness.voice.issueTicket(7);
    expect(harness.voice.consumeTicket(ticket)).toBe(7);
    expect(harness.voice.consumeTicket(ticket)).toBeNull();
  });

  it('认不出的票据返回 null', () => {
    expect(harness.voice.consumeTicket('not-a-ticket')).toBeNull();
    expect(harness.voice.consumeTicket('')).toBeNull();
  });

  it('过期的票据不能再用', () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date('2026-09-22T10:00:00Z'));
    const { ticket, expiresAt } = harness.voice.issueTicket(7);
    expect(expiresAt - Date.now()).toBe(TICKET_TTL_MS);

    vi.setSystemTime(new Date(Date.now() + TICKET_TTL_MS + 1));
    expect(harness.voice.consumeTicket(ticket)).toBeNull();
  });

  it('刚好未过期的票据仍可用', () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date('2026-09-22T10:00:00Z'));
    const { ticket } = harness.voice.issueTicket(7);

    vi.setSystemTime(new Date(Date.now() + TICKET_TTL_MS - 1));
    expect(harness.voice.consumeTicket(ticket)).toBe(7);
  });

  it('两个用户的票据互不串号', () => {
    const alice = harness.voice.issueTicket(1);
    const bob = harness.voice.issueTicket(2);

    expect(harness.voice.consumeTicket(bob.ticket)).toBe(2);
    expect(harness.voice.consumeTicket(alice.ticket)).toBe(1);
  });

  it('每次签发都是不同的票据', () => {
    const first = harness.voice.issueTicket(7);
    const second = harness.voice.issueTicket(7);
    expect(first.ticket).not.toBe(second.ticket);
  });
});
