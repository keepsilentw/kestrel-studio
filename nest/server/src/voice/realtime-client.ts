import { Injectable, Logger } from '@nestjs/common';
import WebSocket, { type RawData } from 'ws';
import type { ResponsesFunctionTool } from '@/bailian/responses-client';
import { resolveApiKey } from '@/bailian/token';
import { loadConfig } from '@/config/configuration';

/** How long to wait for the socket to come up before giving up. */
const CONNECT_TIMEOUT_MS = 10_000;

/**
 * How long to wait for the server to acknowledge a submitted item before
 * proceeding anyway. See `submitToolResult` for why the wait exists.
 */
const ACK_TIMEOUT_MS = 3_000;

/**
 * Normalized session events. Mirrors `StreamEvent` in responses-client.ts: the
 * upstream's own event names leak no further than this file.
 */
export type RealtimeEvent =
  | { kind: 'ready' }
  /** What the user said. */
  | { kind: 'transcript'; text: string }
  /** What the model said, alongside the audio. */
  | { kind: 'assistant_text'; text: string }
  | { kind: 'audio'; pcm: string }
  | { kind: 'function_call'; callId: string; name: string; args: string }
  | { kind: 'response_done' }
  | { kind: 'failed'; message: string }
  | { kind: 'closed'; code: number; reason: string };

export interface OpenOptions {
  instructions: string;
  tools: ResponsesFunctionTool[];
  onEvent: (event: RealtimeEvent) => void;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null;
}

function readString(source: Record<string, unknown>, key: string): string {
  const value = source[key];
  return typeof value === 'string' ? value : '';
}

function readRecord(source: Record<string, unknown>, key: string): Record<string, unknown> | null {
  const value = source[key];
  return isRecord(value) ? value : null;
}

/**
 * `https://host` → `wss://host/api-ws/v1/realtime?model=…`.
 *
 * Derived from the configured HTTP base URL so switching provider moves both
 * surfaces together. A wrong scheme hangs rather than erroring, so it is not
 * hard-coded.
 */
function realtimeUrl(baseUrl: string, model: string): string {
  const url = new URL('/api-ws/v1/realtime', baseUrl);
  url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
  url.searchParams.set('model', model);
  return url.toString();
}

/**
 * One live voice session. Owns the upstream socket and nothing else: the
 * conversation, the tool dispatch and the persistence all live in
 * VoiceService, mirroring how ResponsesClient knows nothing about turns.
 */
export class RealtimeSession {
  private readonly callArgs = new Map<string, string>();
  /** The call whose argument deltas are currently arriving. */
  private activeCallId: string | null = null;
  private assistantText = '';
  private readonly ackWaiters: (() => void)[] = [];
  private closed = false;

  private constructor(
    private readonly socket: WebSocket,
    private readonly options: OpenOptions,
    private readonly logger: Logger,
  ) {}

  static async connect(
    apiKey: string,
    baseUrl: string,
    model: string,
    options: OpenOptions,
    logger: Logger,
  ): Promise<RealtimeSession> {
    const socket = new WebSocket(realtimeUrl(baseUrl, model), {
      headers: { Authorization: `Bearer ${apiKey}` },
    });
    const session = new RealtimeSession(socket, options, logger);
    session.listen();

    await new Promise<void>((resolve, reject) => {
      const failed = (error: Error): void => {
        clearTimeout(timer);
        reject(error);
      };
      const timer = setTimeout(() => {
        reject(new Error('语音会话建立超时'));
      }, CONNECT_TIMEOUT_MS);
      socket.once('open', () => {
        clearTimeout(timer);
        resolve();
      });
      socket.once('error', failed);
    });

    return session;
  }

  /**
   * Pushes one chunk of the utterance: base64 of raw PCM, 16-bit signed, mono,
   * 24 kHz. Verified against the endpoint — a WAV header would be played as
   * audio and garble the utterance rather than being rejected outright.
   */
  appendAudio(pcm: string): void {
    this.send({ type: 'input_audio_buffer.append', audio: pcm });
  }

  /** The button was released; the model takes the turn from here. */
  commit(): void {
    this.send({ type: 'input_audio_buffer.commit' });
  }

  createResponse(): void {
    this.send({ type: 'response.create' });
  }

  /**
   * Hands a tool's result back and waits for the server to acknowledge it.
   *
   * The wait is load-bearing, not politeness: sending `response.create` in the
   * same tick as the result makes the model re-issue the identical tool call
   * instead of continuing. Verified against the endpoint — docs/verification.md §3.2.
   */
  async submitToolResult(callId: string, output: string): Promise<void> {
    if (this.closed) {
      return;
    }
    const acknowledged = new Promise<void>((resolve) => {
      this.ackWaiters.push(resolve);
    });
    this.send({
      type: 'conversation.item.create',
      item: { type: 'function_call_output', call_id: callId, output },
    });

    let timer: NodeJS.Timeout | null = null;
    const gaveUp = new Promise<void>((resolve) => {
      timer = setTimeout(() => {
        this.logger.warn('Timed out waiting for the tool result acknowledgement');
        resolve();
      }, ACK_TIMEOUT_MS);
    });

    await Promise.race([acknowledged, gaveUp]);
    if (timer !== null) {
      clearTimeout(timer);
    }
  }

  close(): void {
    if (this.closed) {
      return;
    }
    this.closed = true;
    this.socket.close();
  }

  private send(payload: Record<string, unknown>): void {
    if (this.closed) {
      return;
    }
    this.socket.send(JSON.stringify(payload));
  }

  private listen(): void {
    this.socket.on('message', (data: RawData) => {
      this.handle(data.toString());
    });
    this.socket.on('error', (error: Error) => {
      this.logger.warn(`Voice socket error: ${error.message}`);
    });
    this.socket.on('close', (code: number, reason: Buffer) => {
      this.closed = true;
      this.options.onEvent({ kind: 'closed', code, reason: reason.toString() });
    });
  }

  private handle(raw: string): void {
    let parsed: unknown;
    try {
      parsed = JSON.parse(raw);
    } catch {
      this.logger.warn('Skipping unparsable frame from the voice endpoint');
      return;
    }
    if (!isRecord(parsed)) {
      return;
    }

    switch (readString(parsed, 'type')) {
      case 'session.created': {
        // Configure on the first event: the socket is up, and this is the
        // earliest point an update is accepted. Voice (`longanqian`) and the
        // transcription model are pinned explicitly rather than left to the
        // server default, because the transcript is load-bearing for the UI.
        this.send({
          type: 'session.update',
          session: {
            modalities: ['audio', 'text'],
            instructions: this.options.instructions,
            tools: this.options.tools,
            tool_choice: 'auto',
            // Push-to-talk is `null`, not 'none' — the latter is rejected.
            turn_detection: null,
            input_audio_transcription: { model: 'fun-asr' },
          },
        });
        break;
      }

      case 'session.updated': {
        this.options.onEvent({ kind: 'ready' });
        break;
      }

      case 'conversation.item.input_audio_transcription.completed': {
        const transcript = readString(parsed, 'transcript');
        if (transcript.length > 0) {
          this.options.onEvent({ kind: 'transcript', text: transcript });
        }
        break;
      }

      case 'response.audio.delta': {
        const delta = readString(parsed, 'delta');
        if (delta.length > 0) {
          this.options.onEvent({ kind: 'audio', pcm: delta });
        }
        break;
      }

      case 'response.audio_transcript.delta': {
        this.assistantText += readString(parsed, 'delta');
        break;
      }

      case 'response.audio_transcript.done': {
        const done = readString(parsed, 'transcript');
        const text = done.length > 0 ? done : this.assistantText;
        this.assistantText = '';
        if (text.length > 0) {
          this.options.onEvent({ kind: 'assistant_text', text });
        }
        break;
      }

      case 'response.output_item.added': {
        const item = readRecord(parsed, 'item');
        if (item !== null && readString(item, 'type') === 'function_call') {
          const callId = readString(item, 'call_id');
          this.callArgs.set(callId, '');
          this.activeCallId = callId;
        }
        break;
      }

      case 'response.function_call_arguments.delta': {
        // The delta carries item_id rather than call_id, so it is routed to the
        // call opened most recently — which is the one whose arguments are
        // arriving, since the server emits `output_item.added` before its deltas.
        const delta = readString(parsed, 'delta');
        if (this.activeCallId !== null) {
          this.callArgs.set(this.activeCallId, (this.callArgs.get(this.activeCallId) ?? '') + delta);
        }
        break;
      }

      case 'response.output_item.done': {
        const item = readRecord(parsed, 'item');
        if (item !== null && readString(item, 'type') === 'function_call') {
          const callId = readString(item, 'call_id');
          const streamed = this.callArgs.get(callId) ?? '';
          this.callArgs.delete(callId);
          if (this.activeCallId === callId) {
            this.activeCallId = null;
          }
          this.options.onEvent({
            kind: 'function_call',
            callId,
            name: readString(item, 'name'),
            args: streamed.length > 0 ? streamed : readString(item, 'arguments') || '{}',
          });
        }
        break;
      }

      case 'conversation.item.created': {
        this.ackWaiters.shift()?.();
        break;
      }

      case 'response.done': {
        this.options.onEvent({ kind: 'response_done' });
        break;
      }

      case 'error': {
        const error = readRecord(parsed, 'error');
        const message =
          (error === null ? '' : readString(error, 'message')) ||
          readString(parsed, 'message') ||
          '语音服务报错';
        this.options.onEvent({ kind: 'failed', message });
        break;
      }

      default:
        break;
    }
  }
}

@Injectable()
export class RealtimeClient {
  private readonly logger = new Logger(RealtimeClient.name);

  /**
   * Opens a session and forwards its events to `onEvent` until it closes.
   * Resolves as soon as the socket is up, so the caller can start feeding audio
   * without waiting for the session to end.
   */
  async open(options: OpenOptions): Promise<RealtimeSession> {
    const { bailian } = loadConfig();
    return RealtimeSession.connect(
      resolveApiKey(),
      bailian.baseUrl,
      bailian.voiceModel,
      options,
      this.logger,
    );
  }
}
