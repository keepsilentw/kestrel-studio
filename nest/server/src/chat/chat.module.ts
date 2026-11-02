import { Module } from '@nestjs/common';
import { AgentModule } from '@/agent/agent.module';
import { AuthModule } from '@/auth/auth.module';
import { ConversationModule } from '@/conversation/conversation.module';
import { TaskModule } from '@/task/task.module';
import { AssetFrameController } from './asset-frame.controller';
import { ChatController } from './chat.controller';

@Module({
  imports: [AgentModule, AuthModule, ConversationModule, TaskModule],
  controllers: [ChatController, AssetFrameController],
})
export class ChatModule {}
