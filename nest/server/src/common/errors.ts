/**
 * Turns a thrown value into text a model can read.
 *
 * Shared by the turn loop and the tool dispatcher: an abort is a normal outcome
 * here (the 120s tool timeout, or the client closing the connection), so it gets
 * a sentence rather than the runtime's "This operation was aborted".
 */
export function describeError(error: unknown): string {
  if (error instanceof Error) {
    return error.name === 'AbortError' ? '请求已取消。' : error.message;
  }
  return String(error);
}
