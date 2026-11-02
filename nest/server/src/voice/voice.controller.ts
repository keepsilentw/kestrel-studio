import { Controller, Post, Req, Res, UseGuards } from '@nestjs/common';
import type { Request, Response } from 'express';
import { AuthenticatedGuard } from '@/auth/guards';
import { VoiceService } from './voice.service';

@Controller('api/voice')
@UseGuards(AuthenticatedGuard)
export class VoiceController {
  constructor(private readonly voice: VoiceService) {}

  /**
   * Trades the session for a single-use ticket.
   *
   * This exists because a WebSocket upgrade never reaches Express, so the
   * session guard cannot run on the socket itself. The browser spends the
   * ticket on the handshake; it expires in a minute and works once.
   */
  @Post('ticket')
  ticket(@Req() req: Request, @Res() res: Response): void {
    const user = req.user;
    if (user === undefined) {
      res.status(401).json({ message: '未登录' });
      return;
    }
    res.json(this.voice.issueTicket(user.id));
  }
}
