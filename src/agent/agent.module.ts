import { Module } from '@nestjs/common';
import { BailianModule } from '@/bailian/bailian.module';
import { ConversationModule } from '@/conversation/conversation.module';
import { MediaModule } from '@/media/media.module';
import { TaskModule } from '@/task/task.module';
import { AgentService } from './agent.service';
import { ToolRegistry } from './tools';

@Module({
  imports: [BailianModule, ConversationModule, MediaModule, TaskModule],
  providers: [AgentService, ToolRegistry],
  // ToolRegistry is exported for the voice session, which dispatches the same
  // tools through a different transport (docs/architecture.md §11.3).
  exports: [AgentService, ToolRegistry],
})
export class AgentModule {}
