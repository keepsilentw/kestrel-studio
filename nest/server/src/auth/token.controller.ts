import {
  Body,
  Controller,
  Delete,
  Get,
  HttpCode,
  Param,
  Post,
  Req,
  Res,
  UnauthorizedException,
  UseGuards,
} from '@nestjs/common';
import type { Request, Response } from 'express';
import { readPositiveInt, readString } from '@/common/http';
import { AuthenticatedGuard } from './guards';
import { TokenService, type IssuedToken, type TokenSummary } from './token.service';

/** Device-token management. P1 exposes the contract; the management screen is P3. */
@Controller('api/v1/tokens')
@UseGuards(AuthenticatedGuard)
export class TokenController {
  constructor(private readonly tokens: TokenService) {}

  @Post()
  @HttpCode(201)
  create(@Req() req: Request, @Body() body: unknown): IssuedToken {
    const user = req.user;
    if (user === undefined) {
      throw new UnauthorizedException('未登录');
    }
    return this.tokens.issue(user.id, readString(body, 'label'));
  }

  @Get()
  list(@Req() req: Request): TokenSummary[] {
    const user = req.user;
    return user === undefined ? [] : this.tokens.list(user.id);
  }

  @Delete(':id')
  revoke(@Req() req: Request, @Param('id') id: string, @Res() res: Response): void {
    const user = req.user;
    const tokenId = readPositiveInt(id);
    if (user === undefined || tokenId === null || !this.tokens.revoke(user.id, tokenId)) {
      res.status(404).json({ message: 'not found' });
      return;
    }
    res.status(200).json({ ok: true });
  }
}
