import { Controller, Get, Param, Query, Res } from '@nestjs/common';
import type { Response } from 'express';
import { readPositiveInt } from '@/common/http';
import { verifySignedFrame } from '@/common/signed-url';
import { ConversationService } from '@/conversation/conversation.service';

/**
 * Session-free asset access for the provider.
 *
 * Image-to-video hands the provider a URL it must fetch on its own, and the
 * normal download endpoint sits behind the session guard. The signed query
 * string is the authorization here, so this controller is deliberately NOT
 * guarded — an HMAC over the asset id plus an expiry, which cannot be replayed
 * or edited into a different asset.
 */
@Controller('api')
export class AssetFrameController {
  constructor(private readonly conversations: ConversationService) {}

  @Get('assets/:id/frame')
  frame(
    @Param('id') id: string,
    @Query('exp') exp: string | undefined,
    @Query('sig') sig: string | undefined,
    @Res() res: Response,
  ): void {
    const assetId = readPositiveInt(id);
    const expiresAt = Number(exp);
    if (
      assetId === null ||
      typeof sig !== 'string' ||
      !verifySignedFrame(assetId, expiresAt, sig)
    ) {
      res.status(403).json({ message: 'forbidden' });
      return;
    }

    const asset = this.conversations.assetFile(assetId);
    if (asset === null) {
      res.status(404).json({ message: 'not found' });
      return;
    }

    res.setHeader('Content-Type', asset.mime);
    res.setHeader('Cache-Control', 'private, max-age=600');
    res.sendFile(asset.filePath, (error?: Error) => {
      if (error !== undefined && !res.headersSent) {
        res.status(404).json({ message: 'not found' });
      }
    });
  }
}
