import { Module } from '@nestjs/common';
import { ConversationModule } from '@/conversation/conversation.module';
import { MediaModule } from '@/media/media.module';
import { TaskService } from './task.service';
import { TaskWorker } from './task.worker';

/**
 * Async provider jobs: create/submit lives in the tools, polling and result
 * delivery live in the worker. Result announcement goes through
 * ConversationEventsService (provided by ConversationModule), which now carries
 * every stream event — background jobs included.
 */
@Module({
  imports: [ConversationModule, MediaModule],
  providers: [TaskService, TaskWorker],
  exports: [TaskService],
})
export class TaskModule {}
