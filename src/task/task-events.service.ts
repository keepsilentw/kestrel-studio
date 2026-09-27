import { Injectable } from '@nestjs/common';
import { Observable, Subject } from 'rxjs';
import type { MessageView } from '@/conversation/conversation.service';
import type { TaskKind, TaskStatus } from './task.service';

export type ConversationEvent =
  | { type: 'task_updated'; taskId: number; kind: TaskKind; status: TaskStatus; error: string | null }
  | { type: 'message_added'; message: MessageView };

/**
 * In-process fan-out for events that happen outside any HTTP turn.
 *
 * A turn's own SSE stream dies with the POST that opened it, but a video render
 * finishes minutes later, possibly with no request in flight. The worker
 * publishes here and every open conversation stream relays it.
 *
 * Single-process only. A second instance would need a real broker — noted in
 * docs/roadmap.md as part of the async task phase.
 */
@Injectable()
export class TaskEventsService {
  private readonly streams = new Map<number, Subject<ConversationEvent>>();

  /**
   * Delivery is synchronous, and a subscriber that throws does NOT take the
   * publisher down: RxJS 7 catches it and re-reports it asynchronously (verified
   * — `next()` returns normally). That is why the task worker needs no guard
   * around this call, and why a broken SSE writer cannot stall a sweep.
   */
  publish(conversationId: number, event: ConversationEvent): void {
    this.streams.get(conversationId)?.next(event);
  }

  subscribe(conversationId: number): Observable<ConversationEvent> {
    let subject = this.streams.get(conversationId);
    if (subject === undefined) {
      subject = new Subject<ConversationEvent>();
      this.streams.set(conversationId, subject);
    }
    const current = subject;

    // Drop the subject once the last listener leaves, so idle conversations do
    // not accumulate map entries for the lifetime of the process.
    return new Observable<ConversationEvent>((subscriber) => {
      const subscription = current.subscribe(subscriber);
      return () => {
        subscription.unsubscribe();
        if (current.observers.length === 0 && this.streams.get(conversationId) === current) {
          this.streams.delete(conversationId);
        }
      };
    });
  }
}
