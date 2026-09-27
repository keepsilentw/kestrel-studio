import { Module } from '@nestjs/common';
import { AgentModule } from '@/agent/agent.module';
import { AuthModule } from '@/auth/auth.module';
import { ConversationModule } from '@/conversation/conversation.module';
import { RealtimeClient } from './realtime-client';
import { VoiceController } from './voice.controller';
import { VoiceGateway } from './voice.gateway';
import { VoiceService } from './voice.service';

/**
 * The voice surface. Separate from ChatModule on purpose: voice is its own
 * entrance rather than a fifth mode (docs/architecture.md §11), and it shares the
 * chat path's tools and conversation store rather than its transport.
 */
@Module({
  imports: [AgentModule, AuthModule, ConversationModule],
  controllers: [VoiceController],
  providers: [VoiceService, VoiceGateway, RealtimeClient],
})
export class VoiceModule {}
