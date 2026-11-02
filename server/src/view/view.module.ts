import { Module } from '@nestjs/common';
import { AgentModule } from '@/agent/agent.module';
import { AuthModule } from '@/auth/auth.module';
import { ConversationModule } from '@/conversation/conversation.module';
import { TaskModule } from '@/task/task.module';
import { ViewController } from './view.controller';

@Module({
  // AgentModule for TurnRunner.catchUpCursor only: the first paint must hand
  // the client the same stream cursor a cursor-less connection would compute.
  imports: [AuthModule, ConversationModule, TaskModule, AgentModule],
  controllers: [ViewController],
})
export class ViewModule {}
