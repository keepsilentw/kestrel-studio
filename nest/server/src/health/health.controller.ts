import {
  Controller,
  Get,
  Inject,
  ServiceUnavailableException,
} from '@nestjs/common';
import Database from 'better-sqlite3';
import { SQLITE_CONNECTION } from '@/database/database.module';

/** The Nest twin of `rust/server/src/http/health.rs`. */
@Controller()
export class HealthController {
  constructor(
    @Inject(SQLITE_CONNECTION) private readonly db: Database.Database,
  ) {}

  @Get('healthz')
  read(): { ok: boolean } {
    try {
      this.db.prepare('SELECT 1').get();
      return { ok: true };
    } catch {
      throw new ServiceUnavailableException({ ok: false });
    }
  }
}
