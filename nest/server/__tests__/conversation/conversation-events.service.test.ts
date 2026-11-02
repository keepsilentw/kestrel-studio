import Database from 'better-sqlite3';
import { drizzle } from 'drizzle-orm/better-sqlite3';
import { describe, expect, it } from 'vitest';
import {
  ConversationEventsService,
  type ConversationEvent,
  type PublishedEvent,
} from '@/conversation/conversation-events.service';
import { applySchema, DRIZZLE_INSTANCE } from '@/database/database.module';
import * as schema from '@/database/schema';

/**
 * The journal is the contract of the whole submit/subscribe split: seq is
 * conversation-scoped and dense (so "cursor below the retained window" is
 * detectable), replay returns events byte-identical to the live ones, and the
 * prune keeps a live turn's own events out of reach of nothing — the newest
 * window is by definition what a reconnecting client still needs.
 */
function textEvent(messageId: number, delta: string): ConversationEvent {
  return { type: 'text', messageId, delta };
}

interface Harness {
  events: ConversationEventsService;
  connection: Database.Database;
  conversationId: number;
}

function buildHarness(): Harness {
  const connection = new Database(':memory:');
  applySchema(connection);
  const db = drizzle(connection, { schema });
  const user = db
    .insert(schema.users)
    .values({ username: 'u', passwordHash: 'x', role: 'user', createdAt: new Date() })
    .run();
  const events = new ConversationEventsService(db);
  const conversationId = Number(
    db
      .insert(schema.conversations)
      .values({
        userId: Number(user.lastInsertRowid),
        title: 'q',
        createdAt: new Date(),
        updatedAt: new Date(),
      })
      .run().lastInsertRowid,
  );
  return { events, connection, conversationId };
}

function collect(events: ConversationEventsService, conversationId: number): PublishedEvent[] {
  const received: PublishedEvent[] = [];
  events.subscribe(conversationId).subscribe((published) => received.push(published));
  return received;
}

describe('ConversationEventsService — 日志与扇出', () => {
  it('publish 先落库再扇出，seq 是会话内连续号', () => {
    const { events, conversationId } = buildHarness();
    const live = collect(events, conversationId);

    const first = events.publish(conversationId, textEvent(7, '一'));
    const second = events.publish(conversationId, textEvent(7, '二'));

    expect(first.seq).toBe(1);
    expect(second.seq).toBe(2);
    // Live delivery is immediate and ordered; the rows are what survives.
    expect(live.map((entry) => entry.seq)).toEqual([1, 2]);
    expect(events.replayAfter(conversationId, 0).map((entry) => entry.seq)).toEqual([1, 2]);
  });

  it('seq 按会话各自计数：第二个会话的第一条也是 1', () => {
    const { events, connection, conversationId } = buildHarness();
    events.publish(conversationId, textEvent(1, 'a'));

    const second = Number(
      connection
        .prepare(
          'INSERT INTO conversations (user_id, title, created_at, updated_at) VALUES (1, ?, ?, ?)',
        )
        .run('第二个会话', Date.now(), Date.now()).lastInsertRowid,
    );
    const published = events.publish(second, textEvent(2, 'b'));

    expect(published.seq).toBe(1);
    expect(events.latestSeq(second)).toBe(1);
    expect(events.latestSeq(conversationId)).toBe(1);
  });

  it('重放与实时逐帧一致：payload 原样存回，含 type', () => {
    const { events, conversationId } = buildHarness();
    const live = collect(events, conversationId);
    const event: ConversationEvent = {
      type: 'task_updated',
      taskId: 9,
      kind: 'video',
      status: 'running',
      error: null,
    };
    events.publish(conversationId, event);

    const replayed = events.replayAfter(conversationId, 0);
    expect(replayed).toHaveLength(1);
    expect(replayed[0]?.event).toEqual(event);
    expect(live[0]?.event).toEqual(replayed[0]?.event);
  });

  it('订阅者收不到自己游标之前的事件之外的东西：replayAfter 只返回 seq > cursor', () => {
    const { events, conversationId } = buildHarness();
    events.publish(conversationId, textEvent(1, 'a'));
    events.publish(conversationId, textEvent(1, 'b'));
    events.publish(conversationId, textEvent(1, 'c'));

    expect(events.replayAfter(conversationId, 2).map((entry) => entry.seq)).toEqual([3]);
    expect(events.replayAfter(conversationId, 3)).toEqual([]);
  });

  it('最后一个监听者离开后扇出主题被回收，不随会话累积', () => {
    const { events, conversationId } = buildHarness();
    const subscription = events.subscribe(conversationId).subscribe(() => undefined);
    subscription.unsubscribe();
    // 再订阅一次仍能工作（主题重建），且没有泄漏的旧主题收到幽灵事件。
    const live = collect(events, conversationId);
    events.publish(conversationId, textEvent(1, 'x'));
    expect(live).toHaveLength(1);
  });
});

describe('ConversationEventsService — 游标语义', () => {
  it('latestSeq / oldestSeq：空日志是 0 与 null，首连因此不算断档', () => {
    const { events, conversationId } = buildHarness();
    expect(events.latestSeq(conversationId)).toBe(0);
    expect(events.oldestSeq(conversationId)).toBeNull();
    expect(events.hasGap(conversationId, 0)).toBe(false);

    events.publish(conversationId, textEvent(1, 'a'));
    expect(events.latestSeq(conversationId)).toBe(1);
    expect(events.oldestSeq(conversationId)).toBe(1);
    expect(events.hasGap(conversationId, 0)).toBe(false);
    expect(events.hasGap(conversationId, 1)).toBe(false);
  });

  it('hasGap 只在游标低于保留窗口时成立——恰好等于 oldest-1 也能完整重放', () => {
    const { events, connection, conversationId } = buildHarness();
    for (let i = 0; i < 5; i += 1) {
      events.publish(conversationId, textEvent(1, String(i)));
    }
    connection
      .prepare('DELETE FROM conversation_events WHERE conversation_id = ? AND seq <= 3')
      .run(conversationId);

    expect(events.oldestSeq(conversationId)).toBe(4);
    expect(events.hasGap(conversationId, 3)).toBe(false);
    expect(events.hasGap(conversationId, 2)).toBe(true);
  });

  it('seqBeforeTurn 指回未完轮次的第一件事之前，用户回声恰好不再重放', () => {
    const { events, conversationId } = buildHarness();
    // 一轮的真实顺序：user_message 带着用户行的 message_id，turn_started 起
    // 才是助手行。
    events.publish(conversationId, {
      type: 'user_message',
      message: {
        id: 101,
        role: 'user',
        content: '问',
        reasoning: null,
        toolCalls: null,
        mode: 'chat',
        createdAt: 0,
        assets: [],
      },
    });
    events.publish(conversationId, { type: 'turn_started', messageId: 102, mode: 'chat' });
    events.publish(conversationId, textEvent(102, '一半'));

    // 助手行(102)的第一件事是 seq 2 → 游标落在 1：重放 2、3，不含 1。
    expect(events.seqBeforeTurn(conversationId, 102)).toBe(1);
    expect(events.replayAfter(conversationId, 1).map((entry) => entry.event.type)).toEqual([
      'turn_started',
      'text',
    ]);
  });

  it('turn 事件没有日志行时（窗口已滑过/从未发出）退回最新 seq', () => {
    const { events, conversationId } = buildHarness();
    expect(events.seqBeforeTurn(conversationId, 999)).toBe(0);
    events.publish(conversationId, textEvent(1, 'a'));
    expect(events.seqBeforeTurn(conversationId, 999)).toBe(1);
  });
});

describe('ConversationEventsService — prune', () => {
  it('保留窗内不动；越窗只剪最老的，最新一条边界不清零', () => {
    const { events, conversationId } = buildHarness();
    // 2000 条的窗口：用 2005 次发布验证边界。内存库足够快。
    for (let i = 1; i <= 2005; i += 1) {
      events.publish(conversationId, textEvent(1, String(i)));
    }
    events.prune(conversationId);

    expect(events.oldestSeq(conversationId)).toBe(6);
    expect(events.latestSeq(conversationId)).toBe(2005);
    expect(events.hasGap(conversationId, 5)).toBe(false);
    expect(events.hasGap(conversationId, 4)).toBe(true);
  });

  it('prune 只裁剪这个会话的行，窗口内的发布不受影响', () => {
    const { events, connection, conversationId } = buildHarness();
    const second = Number(
      connection
        .prepare(
          'INSERT INTO conversations (user_id, title, created_at, updated_at) VALUES (1, ?, ?, ?)',
        )
        .run('other', Date.now(), Date.now()).lastInsertRowid,
    );
    events.publish(conversationId, textEvent(1, 'a'));
    events.publish(second, textEvent(2, 'b'));

    events.prune(conversationId);

    expect(events.oldestSeq(conversationId)).toBe(1);
    expect(events.oldestSeq(second)).toBe(1);
  });
});
