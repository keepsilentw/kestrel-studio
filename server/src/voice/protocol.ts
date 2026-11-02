/**
 * Frames exchanged with the browser over the voice socket.
 *
 * Naming deliberately mirrors the SSE events in `src/common/sse.ts` and the
 * `AssetView` shape, so the two channels can be reasoned about with one mental
 * model. `asset` carries the same fields the SSE `asset` event does.
 *
 * This file is the source of truth for the server side. The browser script
 * cannot import it (server files would drag the `@` alias and Node globals into
 * browser code), so `web/scripts/voice.ts` restates the shapes by hand — keep
 * the two in step.
 */
import type { AssetView } from '@/conversation/conversation.service';

/** Browser → server. */
export type VoiceClientFrame =
  | { type: 'start' }
  /** One chunk of the utterance. Base64 of raw PCM, 16-bit signed, mono, 24 kHz. */
  | { type: 'audio'; pcm: string }
  /** The button was released: the utterance is complete. */
  | { type: 'commit' }
  | { type: 'stop' };

/** Server → browser. */
export type VoiceServerFrame =
  | { type: 'ready'; conversationId: number }
  /** What the user said, after upstream transcription. */
  | { type: 'transcript'; text: string }
  | { type: 'tool_call'; name: string; status: string }
  | ({ type: 'asset' } & AssetView)
  /** Base64 of raw PCM to play back. */
  | { type: 'audio'; pcm: string }
  /** What the model said, as text alongside the audio. */
  | { type: 'assistant_text'; text: string }
  | { type: 'done' }
  | { type: 'error'; message: string };

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null;
}

/**
 * Narrows a raw socket message. Returns null for anything unrecognised rather
 * than throwing: the peer is a browser and a malformed frame must not take the
 * connection down.
 */
export function parseClientFrame(raw: string): VoiceClientFrame | null {
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return null;
  }
  if (!isRecord(parsed)) {
    return null;
  }

  switch (parsed.type) {
    case 'start':
      return { type: 'start' };
    case 'commit':
      return { type: 'commit' };
    case 'stop':
      return { type: 'stop' };
    case 'audio': {
      const pcm = parsed.pcm;
      return typeof pcm === 'string' && pcm.length > 0 ? { type: 'audio', pcm } : null;
    }
    default:
      return null;
  }
}
