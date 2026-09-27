import { Module } from '@nestjs/common';
import { AuthModule } from '@/auth/auth.module';
import { ConversationModule } from '@/conversation/conversation.module';
import { TaskModule } from '@/task/task.module';
import { ViewController } from './view.controller';

@Module({
  imports: [AuthModule, ConversationModule, TaskModule],
  controllers: [ViewController],
})
export class ViewModule {}
