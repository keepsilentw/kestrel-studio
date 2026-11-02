import { Module } from '@nestjs/common';
import { ConversationEventsService } from './conversation-events.service';
import { ConversationService } from './conversation.service';

@Module({
  providers: [ConversationService, ConversationEventsService],
  exports: [ConversationService, ConversationEventsService],
})
export class ConversationModule {}
