import { describe, expect, it } from 'vitest';
import { TaskEventsService, type ConversationEvent } from '@/task/task-events.service';

const taskEvent = (taskId: number): ConversationEvent => ({
  type: 'task_updated',
  taskId,
  kind: 'video',
  status: 'running',
  error: null,
});

interface Collected {
  events: ConversationEvent[];
  /** Detaches this listener. Must be called in tests that assert on teardown. */
  stop(): void;
}

function collect(events: TaskEventsService, conversationId: number): Collected {
  const received: ConversationEvent[] = [];
  const subscription = events
    .subscribe(conversationId)
    .subscribe((event) => received.push(event));
  return { events: received, stop: () => subscription.unsubscribe() };
}

describe('TaskEventsService — 扇出', () => {
  it('发布给本会话的订阅者', () => {
    const events = new TaskEventsService();
    const listener = collect(events, 1);

    events.publish(1, taskEvent(7));
    expect(listener.events).toEqual([taskEvent(7)]);
    listener.stop();
  });

  it('不跨会话投递', () => {
    const events = new TaskEventsService();
    const first = collect(events, 1);
    const second = collect(events, 2);

    events.publish(1, taskEvent(7));

    expect(first.events).toHaveLength(1);
    expect(second.events).toEqual([]);
    first.stop();
    second.stop();
  });

  it('同一会话的多个订阅者都收到', () => {
    const events = new TaskEventsService();
    const a = collect(events, 1);
    const b = collect(events, 1);

    events.publish(1, taskEvent(7));

    expect(a.events).toHaveLength(1);
    expect(b.events).toHaveLength(1);
    a.stop();
    b.stop();
  });

  it('没人订阅时发布不抛', () => {
    // The worker publishes whenever a job moves, whether or not the user still
    // has the page open.
    const events = new TaskEventsService();
    expect(() => events.publish(42, taskEvent(1))).not.toThrow();
  });

  it('退订后不再收到', () => {
    const events = new TaskEventsService();
    const listener = collect(events, 1);

    events.publish(1, taskEvent(1));
    listener.stop();
    events.publish(1, taskEvent(2));

    expect(listener.events).toEqual([taskEvent(1)]);
  });

  it('所有订阅者离开后重新订阅，新事件照常到达', () => {
    // The service drops the subject once the last listener leaves so idle
    // conversations do not hold a map entry for the process lifetime. That
    // deletion is not observable from outside (the map is private), so this
    // only pins the behaviour a caller depends on: resubscribing works.
    const events = new TaskEventsService();

    const first = collect(events, 1);
    events.publish(1, taskEvent(1));
    expect(first.events).toHaveLength(1);
    first.stop();

    const second = collect(events, 1);
    events.publish(1, taskEvent(2));
    expect(second.events).toEqual([taskEvent(2)]);
    second.stop();
  });

  it('事件按发布顺序到达，且是同步的', () => {
    // Synchronous delivery is what lets the task worker publish and move on;
    // the conversation SSE stream relays from the same call stack.
    const events = new TaskEventsService();
    const listener = collect(events, 1);

    events.publish(1, taskEvent(1));
    events.publish(1, taskEvent(2));
    events.publish(1, taskEvent(3));

    expect(listener.events.map((e) => (e.type === 'task_updated' ? e.taskId : -1))).toEqual([
      1, 2, 3,
    ]);
    listener.stop();
  });

  it('message_added 事件原样透传', () => {
    const events = new TaskEventsService();
    const listener = collect(events, 1);
    const message = {
      id: 9,
      role: 'assistant' as const,
      content: '视频好了',
      reasoning: null,
      toolCalls: null,
      mode: 'video' as const,
      createdAt: 1,
      assets: [],
    };

    events.publish(1, { type: 'message_added', message });

    expect(listener.events).toEqual([{ type: 'message_added', message }]);
    listener.stop();
  });
});
