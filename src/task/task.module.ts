import { Module } from '@nestjs/common';
import { ConversationModule } from '@/conversation/conversation.module';
import { MediaModule } from '@/media/media.module';
import { TaskEventsService } from './task-events.service';
import { TaskService } from './task.service';
import { TaskWorker } from './task.worker';

/**
 * Async provider jobs: create/submit lives in the tools, polling and result
 * delivery live in the worker, and the event fan-out is what lets a finished
 * render reach a browser with no request in flight.
 */
@Module({
  imports: [ConversationModule, MediaModule],
  providers: [TaskService, TaskEventsService, TaskWorker],
  exports: [TaskService, TaskEventsService],
})
export class TaskModule {}
