import { Module } from '@nestjs/common';
import { PassportModule } from '@nestjs/passport';
import { AuthController } from './auth.controller';
import { AuthService } from './auth.service';
import { LoginLimiter } from './login-limiter.service';
import { LocalStrategy } from './local.strategy';
import { SessionSerializer } from './session.serializer';
import { TokenController } from './token.controller';
import { TokenService } from './token.service';

@Module({
  imports: [PassportModule.register({ session: true })],
  controllers: [AuthController, TokenController],
  providers: [AuthService, LoginLimiter, LocalStrategy, SessionSerializer, TokenService],
  exports: [AuthService, TokenService],
})
export class AuthModule {}
