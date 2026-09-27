import { Module } from '@nestjs/common';
import { ConversationModule } from '@/conversation/conversation.module';
import { AdminController } from './admin.controller';
import { AdminService } from './admin.service';

/**
 * Account management, gated by SuperAdminGuard. Reads conversations through
 * ConversationService rather than owning any query of its own, so the visibility
 * rule stays in one place (docs/architecture.md §12).
 */
@Module({
  imports: [ConversationModule],
  controllers: [AdminController],
  providers: [AdminService],
})
export class AdminModule {}
