import { Module } from '@nestjs/common';
import { BailianResponsesClient } from './responses-client';

@Module({
  providers: [BailianResponsesClient],
  exports: [BailianResponsesClient],
})
export class BailianModule {}
