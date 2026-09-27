import { marked } from 'marked';
import { MODE_LABEL } from '@/agent/mode';
import type { MessageView } from '@/conversation/conversation.service';

/**
 * Template preparation shared by the two surfaces that render stored turns: the
 * chat page and the super admin's read-only view of someone else's conversation.
 */

export interface RenderedMessage extends MessageView {
  contentHtml: string;
  modeLabel: string | null;
}

/**
 * One entry of the right-hand turn index. A turn is opened by a user message,
 * so that message is both the label and the scroll target.
 */
export interface TurnAnchor {
  messageId: number;
  label: string;
}

/** Assistant text is markdown; templates inject the result with a triple-stash. */
export function renderMessages(views: MessageView[]): RenderedMessage[] {
  return views.map((view) => ({
    ...view,
    contentHtml: view.content.length > 0 ? marked.parse(view.content, { async: false }) : '',
    modeLabel: view.mode === null ? null : (MODE_LABEL[view.mode] ?? null),
  }));
}

export function buildTurnAnchors(views: MessageView[]): TurnAnchor[] {
  return views
    .filter((view) => view.role === 'user' && view.content.trim().length > 0)
    .map((view) => ({ messageId: view.id, label: view.content.trim() }));
}

/**
 * Timestamps for the admin tables. Pinned to the deployment's timezone so a
 * rendered time does not depend on the process's `TZ`.
 */
export function formatTimestamp(ms: number): string {
  return new Date(ms).toLocaleString('zh-CN', {
    timeZone: 'Asia/Shanghai',
    hour12: false,
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  });
}
