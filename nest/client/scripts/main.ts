import { marked } from 'marked';
import '../styles/main.css';

function requireElement<T extends Element>(selector: string): T {
  const element = document.querySelector<T>(selector);
  if (element === null) {
    throw new Error(`Missing element: ${selector}`);
  }
  return element;
}

const body = document.body;
/**
 * Whether this account may hide conversations from its own history. The server
 * decides and states it on <body> — a super admin reads everyone's history and is
 * not offered the control — and the endpoint enforces the same rule.
 */
const canDeleteConversations = body.dataset.canDeleteConversations === 'true';
const stream = requireElement<HTMLElement>('#stream');
const form = requireElement<HTMLFormElement>('#composer-form');
const input = requireElement<HTMLTextAreaElement>('#prompt');
const sendButton = requireElement<HTMLButtonElement>('#send');
const newConversationButton = requireElement<HTMLButtonElement>('#new-conversation');
const previewDialog = requireElement<HTMLDialogElement>('#preview');
const previewImage = requireElement<HTMLImageElement>('#preview-image');

const downloadDialog = requireElement<HTMLDialogElement>('#download-dialog');
const downloadFormat = requireElement<HTMLSelectElement>('#download-format');
const downloadQuality = requireElement<HTMLInputElement>('#download-quality');
const downloadQualityValue = requireElement<HTMLOutputElement>('#download-quality-value');
const downloadHint = requireElement<HTMLElement>('#download-hint');
const downloadEstimate = requireElement<HTMLElement>('#download-estimate');
const downloadConfirm = requireElement<HTMLButtonElement>('#download-confirm');
const downloadCancel = requireElement<HTMLButtonElement>('#download-cancel');

const deleteDialog = requireElement<HTMLDialogElement>('#conversation-delete-dialog');
const deleteDialogTarget = requireElement<HTMLElement>('#conversation-delete-target');

/* ---------- modes ---------- */

type Mode = 'auto' | 'chat' | 'image' | 'video';

const MODE_LABEL: Record<Mode, string> = {
  auto: '智能',
  chat: '对话',
  image: '图片',
  video: '视频',
};

const MODE_STORAGE_KEY = 'kestrel-studio:mode';

function isMode(value: unknown): value is Mode {
  return value === 'auto' || value === 'chat' || value === 'image' || value === 'video';
}

function readStoredMode(): Mode {
  try {
    const raw = window.localStorage.getItem(MODE_STORAGE_KEY);
    return isMode(raw) ? raw : 'auto';
  } catch {
    return 'auto';
  }
}

let currentMode: Mode = readStoredMode();

function paintModeButtons(): void {
  for (const button of document.querySelectorAll<HTMLButtonElement>('.mode-option')) {
    const active = button.dataset.mode === currentMode;
    button.classList.toggle('is-active', active);
    button.setAttribute('aria-checked', active ? 'true' : 'false');
  }
}

function setMode(mode: Mode): void {
  currentMode = mode;
  try {
    window.localStorage.setItem(MODE_STORAGE_KEY, mode);
  } catch {
    // Private mode: the choice just does not survive a reload.
  }
  paintModeButtons();
}

/* ---------- stored messages ---------- */

interface StoredAsset {
  id: number;
  kind: string;
  url: string;
  mime: string;
  bytes: number;
}

interface StoredToolCall {
  name: string;
  summary: string;
}

interface StoredMessage {
  id: number;
  role: 'user' | 'assistant';
  content: string;
  reasoning: string | null;
  toolCalls: StoredToolCall[] | null;
  mode: string | null;
  assets: StoredAsset[];
}

/** Ids already in the DOM, so a replayed event cannot duplicate a turn. */
const renderedMessageIds = new Set<string>();

function rememberRenderedIds(): void {
  for (const element of document.querySelectorAll<HTMLElement>('[data-message-id]')) {
    const id = element.dataset.messageId;
    if (id !== undefined) {
      renderedMessageIds.add(id);
    }
  }
}

function readString(source: Record<string, unknown>, key: string): string {
  const value = source[key];
  return typeof value === 'string' ? value : '';
}

function readNumber(source: Record<string, unknown>, key: string): number {
  const value = source[key];
  return typeof value === 'number' ? value : Number.NaN;
}

function scrollToBottom(): void {
  stream.scrollTop = stream.scrollHeight;
}

function buildModeBadge(mode: Mode): HTMLElement {
  const badge = document.createElement('span');
  badge.className = 'turn-mode';
  badge.textContent = MODE_LABEL[mode];
  return badge;
}

interface AssistantTurn {
  article: HTMLElement;
  reasoningSection: HTMLDetailsElement;
  reasoningBody: HTMLElement;
  content: HTMLElement;
  toolList: HTMLElement;
  assetList: HTMLElement;
}

function buildAssistantTurn(mode: string | null): AssistantTurn {
  const article = document.createElement('article');
  article.className = 'turn turn-assistant';

  const reasoningSection = document.createElement('details');
  reasoningSection.className = 'reasoning';
  reasoningSection.open = true;
  const summary = document.createElement('summary');
  summary.textContent = '思考过程';
  const reasoningBody = document.createElement('pre');
  reasoningBody.className = 'reasoning-body';
  reasoningSection.append(summary, reasoningBody);

  const content = document.createElement('div');
  content.className = 'content';

  const toolList = document.createElement('ul');
  toolList.className = 'tool-calls';

  const assetList = document.createElement('div');
  assetList.className = 'assets';

  if (isMode(mode)) {
    article.append(buildModeBadge(mode));
  }
  article.append(reasoningSection, toolList, content, assetList);
  return { article, reasoningSection, reasoningBody, content, toolList, assetList };
}

function appendAsset(container: HTMLElement, url: string, kind: string): void {
  const figure = document.createElement('figure');
  figure.className = 'asset';

  let media: HTMLElement;
  if (kind === 'video') {
    const video = document.createElement('video');
    video.className = 'asset-video';
    video.src = url;
    video.controls = true;
    video.preload = 'metadata';
    media = video;
  } else {
    const image = document.createElement('img');
    image.className = 'asset-image';
    image.src = url;
    image.alt = '生成的图片';
    media = image;
  }

  const caption = document.createElement('figcaption');
  caption.className = 'asset-actions';
  const link = document.createElement('a');
  link.className = 'asset-download';
  link.href = url;
  link.download = '';
  link.textContent = '下载';
  caption.append(link);

  figure.append(media, caption);
  container.append(figure);
}

/**
 * Renders a message the server sent us in full — a background job's result,
 * which arrives long after the turn that started it.
 */
function renderStoredMessage(message: StoredMessage): HTMLElement {
  const article = document.createElement('article');
  article.className = `turn turn-${message.role}`;
  article.dataset.messageId = String(message.id);

  if (isMode(message.mode)) {
    article.append(buildModeBadge(message.mode));
  }

  if (message.role === 'user') {
    const content = document.createElement('div');
    content.className = 'content';
    content.textContent = message.content;
    article.append(content);
    return article;
  }

  if (message.reasoning !== null && message.reasoning.length > 0) {
    const details = document.createElement('details');
    details.className = 'reasoning';
    const summary = document.createElement('summary');
    summary.textContent = '思考过程';
    const body = document.createElement('pre');
    body.className = 'reasoning-body';
    body.textContent = message.reasoning;
    details.append(summary, body);
    article.append(details);
  }

  if (message.toolCalls !== null && message.toolCalls.length > 0) {
    const list = document.createElement('ul');
    list.className = 'tool-calls';
    for (const call of message.toolCalls) {
      list.append(buildToolCallItem(`${call.name} — ${call.summary}`));
    }
    article.append(list);
  }

  if (message.content.length > 0) {
    const content = document.createElement('div');
    content.className = 'content';
    content.innerHTML = marked.parse(message.content, { async: false });
    article.append(content);
  }

  if (message.assets.length > 0) {
    const list = document.createElement('div');
    list.className = 'assets';
    for (const asset of message.assets) {
      appendAsset(list, asset.url, asset.kind);
    }
    article.append(list);
  }

  return article;
}

function appendStoredMessage(message: StoredMessage, anchor = false): void {
  const key = String(message.id);
  if (renderedMessageIds.has(key)) {
    return;
  }
  renderedMessageIds.add(key);
  const element = renderStoredMessage(message);
  stream.append(element);
  // The right sidebar indexes the conversation by what the user asked. Live
  // user turns used to get their anchor at submit time, when the page was the
  // only source of them; now the stream is, and every client that sees a
  // user_message gets the same row it would have painted itself.
  if (anchor && message.role === 'user') {
    appendTurnAnchor(element, message.content);
  }
  scrollToBottom();
}

/* ---------- turn index (right sidebar) ---------- */

interface TurnAnchorEntry {
  link: HTMLButtonElement;
  target: HTMLElement;
}

const turnAnchorEntries: TurnAnchorEntry[] = [];
let activeAnchor: TurnAnchorEntry | null = null;

/**
 * A conversation may start with no turns at all, in which case the server
 * renders no sidebar — the first live turn has to build it.
 */
function ensureTurnIndexList(): HTMLOListElement {
  const existing = document.querySelector<HTMLOListElement>('#turn-index-list');
  if (existing !== null) {
    return existing;
  }
  const nav = document.createElement('nav');
  nav.className = 'turn-index';
  nav.id = 'turn-index';
  nav.setAttribute('aria-label', '会话节点');
  const title = document.createElement('p');
  title.className = 'turn-index-title';
  title.textContent = '会话节点';
  const list = document.createElement('ol');
  list.className = 'turn-index-list';
  list.id = 'turn-index-list';
  nav.append(title, list);
  // Same place the template puts it: right after the scroll container.
  stream.after(nav);
  return list;
}

function syncTurnIndexVisibility(): void {
  const nav = document.querySelector<HTMLElement>('#turn-index');
  if (nav !== null) {
    nav.hidden = turnAnchorEntries.length === 0;
  }
}

function setActiveAnchor(next: TurnAnchorEntry | null): void {
  if (next === activeAnchor) {
    return;
  }
  activeAnchor?.link.classList.remove('is-active');
  activeAnchor?.link.removeAttribute('aria-current');
  activeAnchor = next;
  if (next !== null) {
    next.link.classList.add('is-active');
    next.link.setAttribute('aria-current', 'true');
    // Keep the highlighted row inside the sidebar's own scroll window.
    next.link.scrollIntoView({ block: 'nearest' });
  }
}

/**
 * The last turn whose top has passed the top of the scroll viewport — i.e. the
 * one currently occupying the top of the reading area.
 *
 * The tolerance must stay above `.turn`'s `scroll-margin-top` (12px in
 * main.css), otherwise a turn just scrolled to sits *below* the threshold and
 * the previous entry stays highlighted.
 */
function updateActiveAnchor(): void {
  if (turnAnchorEntries.length === 0) {
    return;
  }
  const threshold = stream.getBoundingClientRect().top + 24;
  let found: TurnAnchorEntry | null = null;
  for (const entry of turnAnchorEntries) {
    if (entry.target.getBoundingClientRect().top > threshold) {
      break;
    }
    found = entry;
  }
  setActiveAnchor(found ?? turnAnchorEntries[0] ?? null);
}

function bindTurnAnchor(link: HTMLButtonElement, target: HTMLElement): void {
  const entry: TurnAnchorEntry = { link, target };
  link.addEventListener('click', () => {
    target.scrollIntoView({ block: 'start', behavior: 'smooth' });
    setActiveAnchor(entry);
  });
  turnAnchorEntries.push(entry);
}

function appendTurnAnchor(target: HTMLElement, label: string): void {
  const text = label.trim();
  if (text.length === 0) {
    return;
  }
  const item = document.createElement('li');
  const link = document.createElement('button');
  link.type = 'button';
  link.className = 'turn-index-link';
  link.textContent = text;
  link.title = text;
  item.append(link);
  ensureTurnIndexList().append(item);

  bindTurnAnchor(link, target);
  syncTurnIndexVisibility();
  updateActiveAnchor();
}

function clearTurnAnchors(): void {
  turnAnchorEntries.length = 0;
  activeAnchor = null;
  document.querySelector('#turn-index-list')?.replaceChildren();
  syncTurnIndexVisibility();
}

let anchorScrollScheduled = false;

stream.addEventListener('scroll', () => {
  if (anchorScrollScheduled) {
    return;
  }
  anchorScrollScheduled = true;
  window.requestAnimationFrame(() => {
    anchorScrollScheduled = false;
    updateActiveAnchor();
  });
});

/* ---------- conversation event stream ---------- */

let conversationStream: EventSource | null = null;

function closeConversationStream(): void {
  conversationStream?.close();
  conversationStream = null;
}

const TASK_STATUS_TEXT: Record<string, string> = {
  queued: '排队中',
  running: '渲染中',
};

/** The trailing text node is what gets rewritten; the spinner span stays put. */
function setPendingTaskLabel(element: HTMLElement, kind: string, status: string): void {
  const label = `${kind === 'video' ? '视频' : '图片'}任务 #${element.dataset.taskId} ${
    TASK_STATUS_TEXT[status] ?? status
  }…`;
  const text = element.lastChild;
  if (text !== null && text.nodeType === Node.TEXT_NODE) {
    text.textContent = label;
  }
}

function buildPendingTask(taskId: number, kind: string, status: string): HTMLElement {
  const element = document.createElement('div');
  element.className = 'task-pending';
  element.dataset.taskId = String(taskId);
  const spinner = document.createElement('span');
  spinner.className = 'task-spinner';
  spinner.setAttribute('aria-hidden', 'true');
  element.append(spinner, document.createTextNode(''));
  setPendingTaskLabel(element, kind, status);
  return element;
}

/**
 * A job the server announced. Created on first sight: the submit happens
 * mid-turn, so nothing is in the DOM yet — the SSR placeholder only covers a
 * page that was loaded while the job was already running.
 */
function upsertPendingTask(taskId: number, kind: string, status: string): void {
  const existing = document.querySelector<HTMLElement>(`.task-pending[data-task-id="${taskId}"]`);

  if (status === 'succeeded' || status === 'failed') {
    existing?.remove();
    return;
  }
  if (existing !== null) {
    setPendingTaskLabel(existing, kind, status);
    return;
  }
  stream.append(buildPendingTask(taskId, kind, status));
  scrollToBottom();
}

/**
 * The conversation event stream: the page's only view into what the
 * conversation is doing. Turns render entirely from these events — including
 * the one this page just submitted — so a second tab, a reload, and the
 * poster itself all see the same sequence (docs/roadmap.md §2.4.1).
 *
 * EventSource reconnects on its own and sends the last frame's `id:` back as
 * Last-Event-ID; the server replays the journal from there. `from` positions
 * the very first connection: the cursor the page was painted at, or 0 for a
 * conversation created seconds ago (its journal is empty, so "everything from
 * the start" is exactly what the DOM is missing).
 *
 * A render finishing in the background arrives here minutes after its turn;
 * a completion missed while offline also survives, because the worker writes
 * the message into the conversation before announcing it.
 */

interface LiveTurn extends AssistantTurn {
  answer: string;
  pendingTool: HTMLLIElement | null;
}

const liveTurns = new Map<string, LiveTurn>();
/** Terminal outcomes of turns this page has seen, so a send can settle late. */
const finishedTurns = new Map<string, boolean>();
const turnWaiters = new Map<string, (ok: boolean) => void>();

/** A turn from the stream, created on its first event and reused after. */
function ensureLiveTurn(messageId: number, mode: string | null): LiveTurn {
  const key = String(messageId);
  const existing = liveTurns.get(key);
  if (existing !== undefined) {
    return existing;
  }
  const built = buildAssistantTurn(mode);
  built.article.dataset.messageId = key;
  renderedMessageIds.add(key);
  stream.append(built.article);
  const entry: LiveTurn = { ...built, answer: '', pendingTool: null };
  liveTurns.set(key, entry);
  return entry;
}

function streamData(event: Event): Record<string, unknown> | null {
  try {
    const parsed = JSON.parse((event as MessageEvent<string>).data) as unknown;
    return typeof parsed === 'object' && parsed !== null
      ? (parsed as Record<string, unknown>)
      : null;
  } catch {
    return null;
  }
}

/** The assistant message a turn event belongs to, or null to drop the frame. */
function turnKeyOf(data: Record<string, unknown>): string | null {
  const messageId = readNumber(data, 'messageId');
  return Number.isFinite(messageId) ? String(messageId) : null;
}

function turnOf(data: Record<string, unknown>): LiveTurn | null {
  const key = turnKeyOf(data);
  if (key === null) {
    return null;
  }
  // turn_started always precedes the deltas in the journal; the fallback
  // creation only guards against a cursor that started mid-window.
  return ensureLiveTurn(Number(key), null);
}

function finishLiveTurn(turn: LiveTurn): void {
  if (turn.reasoningBody.textContent === '') {
    turn.reasoningSection.remove();
  } else {
    turn.reasoningSection.open = false;
  }
  if (turn.content.textContent === '') {
    turn.content.remove();
  }
}

function settleTurn(key: string, ok: boolean): void {
  finishedTurns.set(key, ok);
  const waiter = turnWaiters.get(key);
  if (waiter !== undefined) {
    turnWaiters.delete(key);
    waiter(ok);
  }
}

/** Wait for a turn's `turn_done` / `turn_error` — possibly already seen. */
function waitForTurn(key: string): Promise<boolean> {
  const settled = finishedTurns.get(key);
  if (settled !== undefined) {
    return Promise.resolve(settled);
  }
  return new Promise<boolean>((resolve) => {
    turnWaiters.set(key, resolve);
  });
}

function showTurnError(message: string): void {
  const paragraph = document.createElement('p');
  paragraph.className = 'turn-error';
  paragraph.textContent = message;
  stream.append(paragraph);
  scrollToBottom();
}

let sendBusy = false;
let streamTurnRunning = false;

/**
 * One composer lock for two reasons: this page has a submit awaiting its
 * terminal event, or the stream says a turn of this conversation is running
 * (possibly started from another tab — and the server enforces one turn per
 * conversation anyway, so unlocking here would just earn a 409).
 */
function syncComposer(): void {
  const locked = sendBusy || streamTurnRunning;
  sendButton.disabled = locked;
  input.disabled = locked;
}

function openConversationStream(conversationId: string, from: string | null): void {
  closeConversationStream();
  liveTurns.clear();
  finishedTurns.clear();
  turnWaiters.clear();
  streamTurnRunning = false;

  const query = from === null || from.length === 0 ? '' : `?from=${encodeURIComponent(from)}`;
  const source = new EventSource(`/api/v1/conversations/${conversationId}/stream${query}`);

  source.addEventListener('user_message', (event) => {
    const data = streamData(event);
    const message = data === null ? undefined : data.message;
    if (typeof message === 'object' && message !== null) {
      appendStoredMessage(message as StoredMessage, true);
    }
  });

  source.addEventListener('turn_started', (event) => {
    const data = streamData(event);
    const key = data === null ? null : turnKeyOf(data);
    if (data === null || key === null) {
      return;
    }
    const mode = typeof data.mode === 'string' ? data.mode : null;
    ensureLiveTurn(Number(key), mode);
    streamTurnRunning = true;
    syncComposer();
  });

  source.addEventListener('reasoning', (event) => {
    const data = streamData(event);
    const turn = data === null ? null : turnOf(data);
    if (data !== null && turn !== null) {
      turn.reasoningBody.append(readString(data, 'delta'));
    }
  });

  source.addEventListener('text', (event) => {
    const data = streamData(event);
    const turn = data === null ? null : turnOf(data);
    if (data !== null && turn !== null) {
      turn.answer += readString(data, 'delta');
      turn.content.innerHTML = marked.parse(turn.answer, { async: false });
    }
  });

  source.addEventListener('tool_call', (event) => {
    const data = streamData(event);
    const turn = data === null ? null : turnOf(data);
    if (data !== null && turn !== null) {
      turn.pendingTool = appendToolCall(turn.toolList, readString(data, 'name'));
    }
  });

  source.addEventListener('tool_result', (event) => {
    const data = streamData(event);
    const turn = data === null ? null : turnOf(data);
    if (data !== null && turn !== null && turn.pendingTool !== null) {
      turn.pendingTool.textContent = `${readString(data, 'name')} — ${readString(data, 'summary')}`;
      turn.pendingTool = null;
    }
  });

  source.addEventListener('asset', (event) => {
    const data = streamData(event);
    const turn = data === null ? null : turnOf(data);
    if (data !== null && turn !== null) {
      appendAsset(turn.assetList, readString(data, 'url'), readString(data, 'kind'));
    }
  });

  source.addEventListener('turn_done', (event) => {
    const data = streamData(event);
    const key = data === null ? null : turnKeyOf(data);
    if (key !== null) {
      const turn = liveTurns.get(key);
      if (turn !== undefined) {
        finishLiveTurn(turn);
        liveTurns.delete(key);
      }
      settleTurn(key, true);
    }
    streamTurnRunning = false;
    syncComposer();
    scrollToBottom();
  });

  source.addEventListener('turn_error', (event) => {
    const data = streamData(event);
    const key = data === null ? null : turnKeyOf(data);
    if (key !== null) {
      const turn = liveTurns.get(key);
      if (turn !== undefined) {
        const error = document.createElement('p');
        error.className = 'turn-error';
        error.textContent = data === null ? '' : readString(data, 'message');
        turn.article.append(error);
        finishLiveTurn(turn);
        liveTurns.delete(key);
      }
      settleTurn(key, false);
    }
    streamTurnRunning = false;
    syncComposer();
  });

  source.addEventListener('message_added', (event) => {
    const data = streamData(event);
    const message = data === null ? undefined : data.message;
    if (typeof message === 'object' && message !== null) {
      appendStoredMessage(message as StoredMessage);
    }
  });

  source.addEventListener('task_updated', (event) => {
    const payload = streamData(event);
    if (payload === null) {
      return;
    }
    upsertPendingTask(readNumber(payload, 'taskId'), readString(payload, 'kind'), readString(payload, 'status'));
  });

  // The cursor this stream asked for is older than the journal's retained
  // window: the missed events cannot be replayed. The persisted rows are
  // complete (the server never journals a fact it does not also store), so a
  // plain reload repaints the truth — the same thing the initial paint does.
  source.addEventListener('resync', () => {
    window.location.reload();
  });

  conversationStream = source;
}

/** Shared by live turns and by messages replayed from the server. */
function buildToolCallItem(text: string): HTMLLIElement {
  const item = document.createElement('li');
  item.className = 'tool-call';
  item.textContent = text;
  return item;
}

function appendToolCall(list: HTMLElement, name: string): HTMLLIElement {
  const item = buildToolCallItem(`${name} 执行中…`);
  list.append(item);
  return item;
}

/**
 * One row of the history dropdown.
 *
 * Mirrors what chat.hbs paints on the initial render — the two must stay in step,
 * or a row changes shape the first time the list refreshes. The delete control is
 * addressed by `data-delete-conversation`, which the delegated listener picks up;
 * the row also has to keep the `.conversation-link` sibling, because that is where
 * the confirmation dialog reads the title it shows.
 */
function buildConversationItem(id: number, title: string, isActive: boolean): HTMLLIElement {
  const item = document.createElement('li');
  item.className = 'conversation-item';

  const link = document.createElement('a');
  link.className = isActive ? 'conversation-link is-active' : 'conversation-link';
  link.href = `/?c=${id}`;
  link.textContent = title;
  item.append(link);

  if (canDeleteConversations) {
    const remove = document.createElement('button');
    remove.type = 'button';
    remove.className = 'conversation-delete';
    remove.dataset.deleteConversation = String(id);
    remove.setAttribute('aria-label', `删除会话 ${title}`);
    remove.title = '删除会话';
    remove.textContent = '删除';
    item.append(remove);
  }

  return item;
}

function buildEmptyConversationItem(): HTMLLIElement {
  const empty = document.createElement('li');
  empty.className = 'conversation-empty';
  empty.textContent = '暂无会话';
  return empty;
}

async function refreshConversationList(): Promise<void> {
  const response = await fetch('/api/v1/conversations');
  if (!response.ok) {
    return;
  }
  const items = (await response.json()) as { id: number; title: string }[];
  const list = document.querySelector<HTMLUListElement>('.conversation-list');
  if (list === null) {
    return;
  }
  list.replaceChildren();
  if (items.length === 0) {
    list.append(buildEmptyConversationItem());
    return;
  }
  const activeId = body.dataset.conversationId ?? '';
  for (const item of items) {
    list.append(buildConversationItem(item.id, item.title, String(item.id) === activeId));
  }
}

/** Surfaces a failure inside the dropdown, where the action came from. */
function showConversationError(message: string): void {
  const list = document.querySelector<HTMLUListElement>('.conversation-list');
  if (list === null) {
    return;
  }
  const item = document.createElement('li');
  item.className = 'conversation-error';
  item.textContent = message;
  list.prepend(item);
}

/** Which conversation the open dialog is about; null once it has been answered. */
let pendingDeleteId: number | null = null;

/**
 * The second step: a modal that names the conversation about to be hidden, so
 * the answer cannot land on the wrong row, and Esc cancels for free. The row is
 * left untouched — there is no state in it to unwind when the question is never
 * answered.
 */
function askToDelete(button: HTMLButtonElement): void {
  const id = Number(button.dataset.deleteConversation ?? '');
  const item = button.closest('li');
  const link = item === null ? null : item.querySelector<HTMLElement>('.conversation-link');
  const title = link?.textContent?.trim() ?? '';
  if (!Number.isInteger(id) || id <= 0 || item === null || title.length === 0) {
    return;
  }

  pendingDeleteId = id;
  deleteDialogTarget.textContent = `「${title}」`;
  deleteDialog.showModal();
}

/**
 * Hides a conversation from the history. Reached only from the confirm dialog,
 * so the deletion is always two clicks.
 */
async function deleteConversation(id: number): Promise<void> {
  const response = await fetch(`/api/v1/conversations/${id}/delete`, { method: 'POST' });
  if (!response.ok) {
    showConversationError(`删除失败（HTTP ${response.status}）`);
    return;
  }

  // Deleting the conversation being read leaves an empty stage, which is the
  // same state 新会话 puts the page in; any other row is just a row going away.
  if (body.dataset.conversationId === String(id)) {
    resetToNewConversation();
    return;
  }
  await refreshConversationList();
}

/** Creates a conversation and opens its stream before anything is posted.
 *
 * `from=0` is load-bearing: EventSource construction is not the same moment
 * as the server seeing the connection, so a turn posted right after this could
 * publish its first events before the stream handler runs. A cursor of 0 makes
 * the handler replay them instead of starting live past them — the ordering
 * the poster used to get for free by reading its own POST response.
 */
async function createConversation(prompt: string): Promise<number> {
  const response = await fetch('/api/v1/conversations', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ prompt }),
  });
  if (!response.ok) {
    throw new Error(`无法创建会话（HTTP ${response.status}）`);
  }
  const created = (await response.json()) as { conversationId: number };
  const conversationId = String(created.conversationId);
  body.dataset.conversationId = conversationId;
  // Keep the address bar in sync so a reload restores this conversation.
  window.history.replaceState(null, '', `${window.location.pathname}?c=${conversationId}`);
  void refreshConversationList();
  openConversationStream(conversationId, '0');
  return created.conversationId;
}

function submitTurn(conversationId: string, prompt: string): Promise<globalThis.Response> {
  return fetch(`/api/v1/conversations/${conversationId}/turns`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({
      prompt,
      mode: currentMode,
    }),
  });
}

/**
 * Submits one turn and waits for its terminal event.
 *
 * Nothing here draws the turn: the user echo, the assistant shell and every
 * delta arrive on the conversation stream, in the journal's order, exactly
 * once — the page is a subscriber like any other (docs/roadmap.md §2.4.1).
 * The POST only answers 202; the turn is no longer this connection's child,
 * which is also why closing the page no longer cancels it.
 */
async function sendPrompt(prompt: string): Promise<void> {
  if (sendBusy || streamTurnRunning) {
    return;
  }
  sendBusy = true;
  syncComposer();

  try {
    let conversationId = body.dataset.conversationId ?? '';
    if (conversationId.length === 0) {
      conversationId = String(await createConversation(prompt));
    }
    let response = await submitTurn(conversationId, prompt);
    if (response.status === 404) {
      // The conversation went away since the page was painted (another tab
      // deleted it). Recreate and retry once; the prompt was never stored.
      conversationId = String(await createConversation(prompt));
      response = await submitTurn(conversationId, prompt);
    }
    if (!response.ok) {
      const detail = (await response.json().catch(() => ({}))) as { message?: unknown };
      showTurnError(
        typeof detail.message === 'string' && detail.message.length > 0
          ? detail.message
          : `提交失败（HTTP ${response.status}）`,
      );
      return;
    }

    const created = (await response.json()) as { conversationId: number; messageId: number };
    // The stream may already have carried the terminal event (its frames race
    // the POST response); waitForTurn settles in that case too.
    await waitForTurn(String(created.messageId));
  } catch (error) {
    showTurnError(error instanceof Error ? error.message : String(error));
  } finally {
    sendBusy = false;
    syncComposer();
    input.focus();
    scrollToBottom();
  }
}

form.addEventListener('submit', (event) => {
  event.preventDefault();
  const prompt = input.value.trim();
  if (prompt.length === 0) {
    return;
  }
  input.value = '';
  input.style.height = 'auto';
  void sendPrompt(prompt);
});

input.addEventListener('keydown', (event) => {
  if (event.key === 'Enter' && !event.shiftKey && !event.isComposing) {
    event.preventDefault();
    form.requestSubmit();
  }
});

input.addEventListener('input', () => {
  input.style.height = 'auto';
  input.style.height = `${Math.min(input.scrollHeight, 200)}px`;
});

/**
 * A <details> popup stays open until its summary is clicked again, which is not
 * what people expect from a dropdown: close it when the click lands outside.
 * Clicks on the summary itself are left alone so the native toggle still works.
 */
document.addEventListener('click', (event) => {
  const dropdown = document.querySelector<HTMLDetailsElement>('.menu-dropdown');
  if (dropdown === null || !dropdown.open) {
    return;
  }
  const target = event.target;
  if (target instanceof Node && !dropdown.contains(target)) {
    dropdown.open = false;
  }
});

/**
 * Bound to the dropdown, not to the document: the dropdown itself is painted by
 * the server and the listener outlives its rows, so it needs no re-binding when
 * the list refreshes — and it does not follow the script onto a page without one.
 */
const historyDropdown = document.querySelector<HTMLElement>('.menu-dropdown');
historyDropdown?.addEventListener('click', (event) => {
  const target = event.target;
  if (!(target instanceof Element)) {
    return;
  }

  const asked = target.closest('button[data-delete-conversation]');
  if (asked instanceof HTMLButtonElement) {
    askToDelete(asked);
  }
});

/**
 * The dialog's two answers. Read off the click rather than through a form with
 * `method="dialog"`, because Esc fires `cancel`/`close` without a click at all
 * and must never be mistaken for 确认.
 */
deleteDialog.addEventListener('click', (event) => {
  const target = event.target;
  if (!(target instanceof Element)) {
    return;
  }

  const choice = target.closest<HTMLButtonElement>('button[data-delete-choice]');
  if (choice === null) {
    return;
  }

  const conversationId = pendingDeleteId;
  // Cleared here rather than left to the `close` event, which the spec queues:
  // one answer must not be able to fire the same request twice.
  pendingDeleteId = null;
  deleteDialog.close();
  if (choice.dataset.deleteChoice === 'confirm' && conversationId !== null) {
    void deleteConversation(conversationId);
  }
});

/** Esc, or any other dismissal, is a cancel: the pending question is dropped. */
deleteDialog.addEventListener('close', () => {
  pendingDeleteId = null;
});

/** Back to the "no conversation yet" state, for 新会话 and for a deleted one. */
function resetToNewConversation(): void {
  delete body.dataset.conversationId;
  stream.replaceChildren();
  closeConversationStream();
  renderedMessageIds.clear();
  liveTurns.clear();
  finishedTurns.clear();
  turnWaiters.clear();
  streamTurnRunning = false;
  clearTurnAnchors();
  syncComposer();
  // Drop ?c=<id>, otherwise a reload re-renders the conversation we just left.
  window.history.replaceState(null, '', window.location.pathname);
  void refreshConversationList();
}

newConversationButton.addEventListener('click', () => {
  resetToNewConversation();
  input.focus();
});

/**
 * Delegated so assets rendered on the server (initial paint) work too; binding
 * per-node only covered the ones the client created itself.
 */
stream.addEventListener('click', (event) => {
  const target = event.target;
  if (!(target instanceof Element)) {
    return;
  }

  const image = target.closest('img.asset-image');
  if (image instanceof HTMLImageElement) {
    previewImage.src = image.src;
    previewDialog.showModal();
    return;
  }

  const link = target.closest('a.asset-download');
  if (link instanceof HTMLAnchorElement) {
    // The re-encode dialog is image-only; let the browser fetch a video as-is.
    const figure = link.closest('figure.asset');
    if (figure !== null && figure.querySelector('video') !== null) {
      return;
    }
    event.preventDefault();
    void openDownloadDialog(link.href);
  }
});

previewDialog.addEventListener('click', () => {
  previewDialog.close();
  previewImage.src = '';
});

/* ---------- download dialog ---------- */

interface DownloadSource {
  image: HTMLImageElement | null;
}

let downloadSource: DownloadSource = { image: null };
let estimateTimer: number | null = null;

function loadImage(url: string): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const image = new Image();
    image.addEventListener('load', () => resolve(image));
    image.addEventListener('error', () => reject(new Error('图片加载失败')));
    image.src = url;
  });
}

/** Falls back to the markup's default (100) if the radio group ever goes empty. */
function selectedSize(): number {
  const checked = document.querySelector<HTMLInputElement>('input[name="download-size"]:checked');
  return checked === null ? 100 : Number(checked.value);
}

/** Re-encoding only exists for lossy formats; PNG ignores the quality argument. */
function qualityApplies(): boolean {
  return downloadFormat.value !== 'image/png';
}

function currentQuality(): number {
  return qualityApplies() ? Number(downloadQuality.value) / 100 : 1;
}

/** Scales the longest edge to `size` while preserving the aspect ratio. */
async function renderBlob(
  image: HTMLImageElement,
  size: number,
  format: string,
  quality: number,
): Promise<Blob> {
  const scale = size / Math.max(image.naturalWidth, image.naturalHeight);
  const width = Math.max(1, Math.round(image.naturalWidth * scale));
  const height = Math.max(1, Math.round(image.naturalHeight * scale));

  const canvas = document.createElement('canvas');
  canvas.width = width;
  canvas.height = height;

  const context = canvas.getContext('2d');
  if (context === null) {
    throw new Error('当前浏览器不支持 canvas');
  }
  context.drawImage(image, 0, 0, width, height);

  return new Promise((resolve, reject) => {
    canvas.toBlob(
      (blob) => {
        if (blob === null) {
          reject(new Error('图片导出失败'));
          return;
        }
        resolve(blob);
      },
      format,
      quality,
    );
  });
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) {
    return `${bytes} B`;
  }
  if (bytes < 1024 * 1024) {
    return `${(bytes / 1024).toFixed(1)} KB`;
  }
  return `${(bytes / 1024 / 1024).toFixed(2)} MB`;
}

function syncQualityAvailability(): void {
  const applies = qualityApplies();
  downloadQuality.disabled = !applies;
  downloadQualityValue.textContent = applies ? `${downloadQuality.value}%` : '不适用';
  downloadHint.textContent = applies
    ? '质量越低文件越小，画质损失越明显。'
    : 'PNG 为无损格式，质量选项不生效，请用尺寸控制体积。';
}

async function updateEstimate(): Promise<void> {
  const image = downloadSource.image;
  if (image === null) {
    downloadEstimate.textContent = '预计大小：无法计算';
    return;
  }
  try {
    const blob = await renderBlob(image, selectedSize(), downloadFormat.value, currentQuality());
    downloadEstimate.textContent = `预计大小：${formatBytes(blob.size)}`;
  } catch (error) {
    downloadEstimate.textContent = `预计大小：${error instanceof Error ? error.message : '计算失败'}`;
  }
}

/** Re-encoding is not free; coalesce rapid slider input into a single pass. */
function scheduleEstimate(): void {
  if (estimateTimer !== null) {
    window.clearTimeout(estimateTimer);
  }
  estimateTimer = window.setTimeout(() => {
    estimateTimer = null;
    void updateEstimate();
  }, 160);
}

async function openDownloadDialog(url: string): Promise<void> {
  downloadSource = { image: null };
  syncQualityAvailability();
  downloadEstimate.textContent = '预计大小：计算中…';
  downloadDialog.showModal();

  try {
    downloadSource.image = await loadImage(url);
    await updateEstimate();
  } catch (error) {
    downloadEstimate.textContent = `预计大小：${error instanceof Error ? error.message : '加载失败'}`;
  }
}

async function performDownload(): Promise<void> {
  const image = downloadSource.image;
  if (image === null) {
    throw new Error('图片尚未加载完成');
  }

  const size = selectedSize();
  const format = downloadFormat.value;
  const blob = await renderBlob(image, size, format, currentQuality());

  const extension = format === 'image/png' ? 'png' : format === 'image/jpeg' ? 'jpg' : 'webp';
  const objectUrl = URL.createObjectURL(blob);
  const link = document.createElement('a');
  link.href = objectUrl;
  link.download = `kestrel-${size}-${Date.now()}.${extension}`;
  link.click();
  URL.revokeObjectURL(objectUrl);
}

for (const radio of document.querySelectorAll('input[name="download-size"]')) {
  radio.addEventListener('change', scheduleEstimate);
}

downloadFormat.addEventListener('change', () => {
  syncQualityAvailability();
  scheduleEstimate();
});

downloadQuality.addEventListener('input', () => {
  downloadQualityValue.textContent = `${downloadQuality.value}%`;
  scheduleEstimate();
});

downloadCancel.addEventListener('click', () => {
  downloadDialog.close();
});

downloadConfirm.addEventListener('click', () => {
  void performDownload()
    .then(() => downloadDialog.close())
    .catch((error: unknown) => {
      downloadEstimate.textContent = error instanceof Error ? error.message : '下载失败';
    });
});

downloadDialog.addEventListener('click', (event) => {
  if (event.target === downloadDialog) {
    downloadDialog.close();
  }
});

/* ---------- init ---------- */

for (const button of document.querySelectorAll<HTMLButtonElement>('.mode-option')) {
  button.addEventListener('click', () => {
    const mode = button.dataset.mode;
    if (isMode(mode)) {
      setMode(mode);
    }
  });
}
paintModeButtons();

rememberRenderedIds();

// Sidebar entries the server rendered; bind each to the turn it points at.
for (const link of document.querySelectorAll<HTMLButtonElement>('.turn-index-link[data-target]')) {
  const id = link.dataset.target;
  const target =
    id === undefined ? null : document.querySelector<HTMLElement>(`[data-message-id="${id}"]`);
  if (target !== null) {
    bindTurnAnchor(link, target);
  }
}
updateActiveAnchor();

const initialConversationId = body.dataset.conversationId;
if (initialConversationId !== undefined && initialConversationId.length > 0) {
  // The server painted this page from the rows up to startSeq; the stream
  // continues from exactly there — before a still-running turn's first event,
  // or at the newest one if nothing is in flight.
  openConversationStream(initialConversationId, body.dataset.startSeq ?? null);
}

input.focus();
