/**
 * The sentence a person sees when the daily asset cap stops a generation. Kept
 * here because both the image tool and the video worker produce it, and the Rust
 * side prints the same words (`AppState::daily_limit_message`).
 */
export function dailyLimitMessage(limit: number): string {
  return `生成额度已用完：每账号 24 小时内最多 ${limit} 个生成结果，请等窗口过去后再试。`;
}
