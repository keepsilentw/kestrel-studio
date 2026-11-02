import { Module } from '@nestjs/common';
import { AdminModule } from '@/admin/admin.module';
import { AgentModule } from '@/agent/agent.module';
import { AuthModule } from '@/auth/auth.module';
import { ChatModule } from '@/chat/chat.module';
import { ConversationModule } from '@/conversation/conversation.module';
import { DatabaseModule } from '@/database/database.module';
import { MediaModule } from '@/media/media.module';
import { TaskModule } from '@/task/task.module';
import { ViewModule } from '@/view/view.module';
import { VoiceModule } from '@/voice/voice.module';

@Module({
  imports: [
    DatabaseModule,
    AuthModule,
    ConversationModule,
    MediaModule,
    TaskModule,
    AgentModule,
    ChatModule,
    VoiceModule,
    ViewModule,
    AdminModule,
  ],
})
export class AppModule {}
