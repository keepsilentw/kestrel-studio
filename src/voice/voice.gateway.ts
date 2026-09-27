import {
  Injectable,
  Logger,
  type OnApplicationBootstrap,
  type OnApplicationShutdown,
} from '@nestjs/common';
import { HttpAdapterHost } from '@nestjs/core';
import { Server, type IncomingMessage } from 'node:http';
import type { Duplex } from 'node:stream';
import { WebSocketServer } from 'ws';
import { VoiceService } from './voice.service';

/** The socket path. The page that drives it is `GET /voice`. */
const VOICE_PATH = '/api/voice';
const TICKET_PARAM = 'ticket';

/**
 * Takes over the HTTP upgrade for the voice socket.
 *
 * Attached from `onApplicationBootstrap` rather than from main.ts, for two
 * reasons: the socket is reachable in tests (a testing module calls `init()`,
 * so this runs against a test server), and `app.listen()` is `init()` plus
 * `listen()`, so the handler is in place before anything can connect.
 */
@Injectable()
export class VoiceGateway implements OnApplicationBootstrap, OnApplicationShutdown {
  private readonly logger = new Logger(VoiceGateway.name);
  private readonly sockets = new WebSocketServer({ noServer: true });

  constructor(
    private readonly adapterHost: HttpAdapterHost,
    private readonly voice: VoiceService,
  ) {}

  onApplicationBootstrap(): void {
    const server: unknown = this.adapterHost.httpAdapter.getHttpServer();
    if (!(server instanceof Server)) {
      this.logger.warn('No HTTP server to attach the voice socket to');
      return;
    }

    // This is the only upgrade listener, so every upgrade has to be terminated
    // here. Returning without answering leaves the socket open until it times
    // out, which a stray client would see as a hang rather than a refusal.
    server.on('upgrade', (request: IncomingMessage, socket: Duplex, head: Buffer) => {
      this.upgrade(request, socket, head);
    });
  }

  onApplicationShutdown(): void {
    this.sockets.close();
  }

  private upgrade(request: IncomingMessage, socket: Duplex, head: Buffer): void {
    const url = new URL(request.url ?? '/', 'http://localhost');
    if (url.pathname !== VOICE_PATH) {
      this.refuse(socket, 404, 'Not Found');
      return;
    }

    const ticket = url.searchParams.get(TICKET_PARAM);
    const userId = ticket === null ? null : this.voice.consumeTicket(ticket);
    if (userId === null) {
      this.refuse(socket, 401, 'Unauthorized');
      return;
    }

    this.sockets.handleUpgrade(request, socket, head, (client) => {
      this.voice.attach(client, userId);
    });
  }

  /** The upgrade never completes, so the response is written by hand. */
  private refuse(socket: Duplex, status: number, reason: string): void {
    socket.write(`HTTP/1.1 ${status} ${reason}\r\nConnection: close\r\nContent-Length: 0\r\n\r\n`);
    socket.destroy();
  }
}
