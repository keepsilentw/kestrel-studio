import { Injectable, Logger } from '@nestjs/common';
import { describeError } from '@/common/errors';
import { randomUUID } from 'node:crypto';
import WebSocket from 'ws';
import { describeOutcome, ToolRegistry } from '@/agent/tools';
import { parseToolArguments } from '@/common/args';
import {
  assetUrl,
  ConversationService,
  type ToolCallRecord,
} from '@/conversation/conversation.service';
import type { StoredAsset } from '@/media/media.service';
import { parseClientFrame, type VoiceClientFrame, type VoiceServerFrame } from './protocol';
import { RealtimeClient, type RealtimeEvent, type RealtimeSession } from './realtime-client';

/**
 * A ticket only has to outlive the redirect from the HTTP request that issues it
 * to the socket handshake that spends it.
 */
const TICKET_TTL_MS = 60_000;

/** Bounds a tool loop that never converges, as MAX_TOOL_ROUNDS does for text. */
const MAX_TOOL_ROUNDS = 5;

/** How long to wait for a transcript before assuming the utterance was silent. */
const SILENCE_TIMEOUT_MS = 12_000;

/**
 * Instructions for a spoken assistant.
 *
 * Written for speech rather than for a transcript: no markdown, no URLs, short
 * replies — anything else reads badly once synthesized.
 */
const VOICE_INSTRUCTIONS = [
  '你是一个生成式媒体助手，正在通过语音与用户实时对话。',
  '',
  '规则：',
  '- 用中文口语回答，简短自然，一次说一两句。',
  '- 生成的图片或视频会自动显示在用户屏幕上，不要念出链接，也不要描述文件格式。',
  '- 开始生成前先用一句话说明你要画什么。',
].join('\n');

/** Shown in history when a turn produced neither speech nor a tool call. */
const EMPTY_TURN_NOTE = '（本轮没有语音输出）';

interface IssuedTicket {
  userId: number;
  expiresAt: number;
}

@Injectable()
export class VoiceService {
  private readonly logger = new Logger(VoiceService.name);
  private readonly tickets = new Map<string, IssuedTicket>();

  constructor(
    private readonly realtime: RealtimeClient,
    private readonly tools: ToolRegistry,
    private readonly conversations: ConversationService,
  ) {}

  /**
   * Mints a single-use ticket for one browser connection.
   *
   * A WebSocket upgrade never reaches Express, so the session guard cannot run
   * on it. Rather than reach into the session store from the upgrade path, the
   * authenticated HTTP request trades its session for a short-lived ticket —
   * the same shape as the signed asset URL in common/signed-url.ts, which
   * exists for the same reason (the guard cannot cover the caller).
   */
  issueTicket(userId: number): { ticket: string; expiresAt: number } {
    this.purgeExpired();
    const ticket = randomUUID();
    const expiresAt = Date.now() + TICKET_TTL_MS;
    this.tickets.set(ticket, { userId, expiresAt });
    return { ticket, expiresAt };
  }

  /** Spends a ticket. Returns the user it was minted for, or null. */
  consumeTicket(ticket: string): number | null {
    const issued = this.tickets.get(ticket);
    if (issued === undefined) {
      return null;
    }
    this.tickets.delete(ticket);
    return issued.expiresAt < Date.now() ? null : issued.userId;
  }

  /** Wires a freshly upgraded socket to a new connection. */
  attach(socket: WebSocket, userId: number): void {
    const connection = new VoiceConnection(
      socket,
      userId,
      this.realtime,
      this.tools,
      this.conversations,
    );

    socket.on('message', (data) => {
      void connection.receive(data.toString());
    });
    socket.on('close', () => {
      connection.close();
    });
    socket.on('error', (error: Error) => {
      this.logger.warn(`Voice socket error: ${error.message}`);
      connection.close();
    });
  }

  private purgeExpired(): void {
    const now = Date.now();
    for (const [ticket, issued] of this.tickets) {
      if (issued.expiresAt < now) {
        this.tickets.delete(ticket);
      }
    }
  }
}

/**
 * One live call. Owns the per-call state machine: it opens a conversation, runs
 * the tool round trip, writes both sides of the conversation down, and relays
 * audio. Instantiated per socket, like SseWriter is per request.
 *
 * Note that voice turns are *not* replayed to the model the way text turns are:
 * the upstream session keeps its own context for the life of the call. What is
 * persisted here is history and UI, not model input.
 */
class VoiceConnection {
  private readonly logger = new Logger(VoiceConnection.name);
  private readonly abort = new AbortController();
  private readonly toolCalls: ToolCallRecord[] = [];
  /** Calls collected during the current response, dispatched on `response.done`. */
  private readonly pendingCalls: { callId: string; name: string; args: string }[] = [];

  private session: RealtimeSession | null = null;
  private conversationId: number | null = null;
  private assistantMessageId: number | null = null;
  private assistantText = '';
  private toolRounds = 0;
  /** Audio frames received since the last commit, logged with each turn. */
  private audioFrames = 0;
  /** Upstream failures in the current turn, so a run of them says so once. */
  private failures = 0;
  /** The endpoint refused this utterance; stop feeding it until the next commit. */
  private refused = false;
  /** Fires when an utterance produced no transcript at all. */
  private silenceTimer: NodeJS.Timeout | null = null;
  /** Serializes tool dispatch so two rounds cannot interleave. */
  private work: Promise<void> = Promise.resolve();
  /** A turn is in flight; further utterances are refused until it settles. */
  private busy = false;
  private closed = false;

  constructor(
    private readonly socket: WebSocket,
    private readonly userId: number,
    private readonly realtime: RealtimeClient,
    private readonly tools: ToolRegistry,
    private readonly conversations: ConversationService,
  ) {}

  async receive(raw: string): Promise<void> {
    const frame = parseClientFrame(raw);
    if (frame === null) {
      this.logger.warn('Ignoring an unparsable frame from the browser');
      return;
    }

    switch (frame.type) {
      case 'start':
        await this.start();
        break;
      case 'audio':
        this.audioFrames += 1;
        // Once the endpoint has refused this utterance, appending more of it
        // only earns more refusals — which is how one buffer overflow became a
        // stream of errors. The next commit starts a fresh buffer.
        if (!this.refused) {
          this.session?.appendAudio(frame.pcm);
        }
        break;
      case 'commit':
        if (this.busy) {
          // The UI disables the button until `done`, so this is a stray press
          // rather than a normal path — dropping it keeps one turn per utterance.
          this.logger.warn('Ignoring an utterance while a turn is still running');
          break;
        }
        this.busy = true;
        this.refused = false;
        this.session?.commit();
        // The frame count is what tells "the microphone sent nothing" apart from
        // "the endpoint rejected what it sent" — the two look identical from the
        // browser, and both end in an error line.
        this.logger.log(`Voice turn committed (${this.audioFrames} audio frames)`);
        this.audioFrames = 0;
        this.armSilenceGuard();
        break;
      case 'stop':
        this.close();
        break;
    }
  }

  close(): void {
    if (this.closed) {
      return;
    }
    this.closed = true;
    this.logger.log(`Voice call ended (conversation ${this.conversationId ?? '未建立'})`);
    this.abort.abort();
    this.session?.close();
    this.session = null;
    // Never leave an unfinalised assistant row: an orphaned placeholder is what
    // made a later turn replay two user messages back to back in the text path
    // (see the incident note in agent.service.ts).
    this.finalizeTurn();
    if (this.socket.readyState === WebSocket.OPEN) {
      this.socket.close();
    }
  }

  private async start(): Promise<void> {
    if (this.session !== null) {
      return;
    }
    const conversationId = this.conversations.create(this.userId, '语音会话');
    this.conversationId = conversationId;

    try {
      this.session = await this.realtime.open({
        instructions: VOICE_INSTRUCTIONS,
        tools: this.tools.allSpecs(),
        onEvent: (event) => this.onRealtime(event),
      });
      // Both ends of this call are logged: the failure that made this necessary
      // was a client that never sent `start`, and the server — silent on the
      // success path and only warning on a few errors — left nothing to read.
      this.logger.log(
        `Voice session opened (conversation ${conversationId}, user ${this.userId})`,
      );
    } catch (error) {
      this.logger.warn(`Voice session failed: ${describeError(error)}`);
      this.send({
        type: 'error',
        message: error instanceof Error ? error.message : '语音会话建立失败',
      });
      this.close();
    }
  }

  private onRealtime(event: RealtimeEvent): void {
    switch (event.kind) {
      case 'ready': {
        if (this.conversationId !== null) {
          this.send({ type: 'ready', conversationId: this.conversationId });
        }
        break;
      }

      case 'transcript': {
        this.openTurn(event.text);
        // Push-to-talk does not auto-answer: with `turn_detection: null` the
        // server commits the audio but waits to be asked. Without this the call
        // goes quiet after the transcript and the button stays held.
        this.session?.createResponse();
        break;
      }

      case 'function_call': {
        // Collected, not executed: one response can carry several calls, and
        // acting on the first would send `response.create` before the rest are
        // answered. Dispatch happens on `response.done`.
        this.pendingCalls.push({ callId: event.callId, name: event.name, args: event.args });
        this.send({ type: 'tool_call', name: event.name, status: 'running' });
        break;
      }

      case 'assistant_text': {
        // Trimmed per segment: the upstream's transcript carries a trailing
        // newline, and joining those verbatim leaves blank lines in history.
        const text = event.text.trim();
        if (text.length === 0) {
          break;
        }
        this.assistantText =
          this.assistantText.length > 0 ? `${this.assistantText}\n${text}` : text;
        this.send({ type: 'assistant_text', text });
        break;
      }

      case 'audio': {
        this.send({ type: 'audio', pcm: event.pcm });
        break;
      }

      case 'response_done': {
        if (this.pendingCalls.length > 0) {
          this.work = this.work.then(() => this.dispatchPending());
        } else {
          this.finalizeTurn();
        }
        break;
      }

      case 'failed': {
        this.failures += 1;
        this.refused = true;
        // The endpoint's own words. They reach the browser and are then gone, so
        // a call that starts failing leaves no trace on the server at all —
        // which is exactly the incident this line exists to prevent.
        this.logger.warn(
          `Voice turn failed: ${event.message}` +
            (this.failures > 1 ? ` (${this.failures} this turn)` : ''),
        );
        // One line in the UI per turn, not one per rejected frame: a run of
        // failures is one problem, and the page can only show so many.
        if (this.failures === 1) {
          this.send({ type: 'error', message: event.message });
        }
        // The turn is over even though it failed; leaving it open would hold the
        // button disabled for the rest of the call.
        this.finalizeTurn();
        break;
      }

      case 'closed': {
        if (event.code !== 1000 && !this.closed) {
          this.logger.warn(`Voice session closed by the endpoint (${event.code})`);
        }
        this.close();
        break;
      }
    }
  }

  /**
   * An utterance too quiet to transcribe produces no transcript, so no turn
   * opens and the button would stay held for the rest of the call. Releases it
   * instead, and says why.
   */
  private armSilenceGuard(): void {
    if (this.silenceTimer !== null) {
      clearTimeout(this.silenceTimer);
    }
    this.silenceTimer = setTimeout(() => {
      this.silenceTimer = null;
      if (this.busy && this.assistantMessageId === null) {
        this.busy = false;
        this.logger.warn('Voice turn produced no transcript (silence guard)');
        this.send({ type: 'error', message: '没有听清，请再说一次' });
      }
    }, SILENCE_TIMEOUT_MS);
  }

  private openTurn(transcript: string): void {
    if (this.silenceTimer !== null) {
      clearTimeout(this.silenceTimer);
      this.silenceTimer = null;
    }
    this.failures = 0;
    this.logger.log(`Voice transcript: ${transcript.trim()}`);
    if (this.conversationId === null) {
      return;
    }
    this.conversations.appendUserMessage(this.conversationId, transcript, null);
    this.assistantMessageId = this.conversations.createAssistantPlaceholder(
      this.conversationId,
      null,
    );
    this.assistantText = '';
    this.toolCalls.length = 0;
    this.toolRounds = 0;
    this.send({ type: 'transcript', text: transcript });
  }

  private async dispatchPending(): Promise<void> {
    const session = this.session;
    const conversationId = this.conversationId;
    const messageId = this.assistantMessageId;
    const calls = this.pendingCalls.splice(0);
    if (session === null || conversationId === null || messageId === null) {
      return;
    }

    this.toolRounds += 1;
    const results: { callId: string; output: string }[] = [];

    for (const call of calls) {
      const args = parseToolArguments(call.args);
      const outcome = await this.tools.execute(call.name, args, {
        conversationId,
        messageId,
        signal: this.abort.signal,
      });

      const assetIds: number[] = [];
      for (const asset of outcome.assets) {
        const assetId = this.conversations.addAsset(messageId, asset);
        assetIds.push(assetId);
        this.sendAsset(assetId, asset);
      }

      const summary = describeOutcome(outcome, assetIds);
      this.toolCalls.push({ name: call.name, arguments: args, ok: outcome.ok, summary });
      this.send({
        type: 'tool_call',
        name: call.name,
        status: outcome.ok ? 'done' : 'failed',
      });
      results.push({ callId: call.callId, output: summary });
    }

    // Every result goes back before the continuation is requested, and each
    // submit waits for its acknowledgement — see RealtimeSession.submitToolResult.
    for (const result of results) {
      await session.submitToolResult(result.callId, result.output);
    }

    if (this.toolRounds >= MAX_TOOL_ROUNDS) {
      this.logger.warn(`Voice turn hit the tool round cap (${MAX_TOOL_ROUNDS})`);
      this.send({ type: 'error', message: '工具调用轮次过多，已停止' });
      this.finalizeTurn();
      return;
    }
    session.createResponse();
  }

  private finalizeTurn(): void {
    const messageId = this.assistantMessageId;
    const conversationId = this.conversationId;
    this.busy = false;
    if (this.silenceTimer !== null) {
      clearTimeout(this.silenceTimer);
      this.silenceTimer = null;
    }
    if (messageId === null || conversationId === null) {
      return;
    }
    this.assistantMessageId = null;

    this.conversations.finalizeAssistant(messageId, {
      content:
        this.assistantText.length > 0
          ? this.assistantText
          : this.toolCalls.length > 0
            ? ''
            : EMPTY_TURN_NOTE,
      reasoning: null,
      toolCalls: this.toolCalls.length > 0 ? this.toolCalls : null,
    });
    this.conversations.touch(conversationId);
    this.send({ type: 'done' });
  }

  private sendAsset(assetId: number, asset: StoredAsset): void {
    this.send({
      type: 'asset',
      id: assetId,
      kind: asset.kind,
      url: assetUrl(assetId),
      mime: asset.mime,
      bytes: asset.bytes,
    });
  }

  private send(frame: VoiceServerFrame): void {
    if (this.closed || this.socket.readyState !== WebSocket.OPEN) {
      return;
    }
    this.socket.send(JSON.stringify(frame));
  }
}
