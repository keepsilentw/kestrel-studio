import { Module } from '@nestjs/common';
import { BailianModule } from '@/bailian/bailian.module';
import { ConversationModule } from '@/conversation/conversation.module';
import { MediaModule } from '@/media/media.module';
import { TaskModule } from '@/task/task.module';
import { AgentService } from './agent.service';
import { ToolRegistry } from './tools';
import { TurnRunner } from './turn-runner.service';

@Module({
  imports: [BailianModule, ConversationModule, MediaModule, TaskModule],
  providers: [AgentService, ToolRegistry, TurnRunner],
  // ToolRegistry is exported for the voice session, which dispatches the same
  // tools through a different transport (docs/architecture.md §11.3).
  // TurnRunner is exported for the HTTP surface, which submits turns but never
  // runs them on its own connection (docs/roadmap.md §2.4.1).
  exports: [AgentService, ToolRegistry, TurnRunner],
})
export class AgentModule {}
