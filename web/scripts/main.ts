import { marked } from 'marked';
import '../styles/main.css';

interface SseFrame {
  event: string;
  data: Record<string, unknown>;
}

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

function parseFrame(raw: string): SseFrame | null {
  let event: string | null = null;
  const dataLines: string[] = [];
  for (const line of raw.split('\n')) {
    if (line.length === 0 || line.startsWith(':')) {
      continue;
    }
    if (line.startsWith('event:')) {
      event = line.slice('event:'.length).trim();
    } else if (line.startsWith('data:')) {
      dataLines.push(line.slice('data:'.length).replace(/^ /, ''));
    }
  }
  if (event === null || dataLines.length === 0) {
    return null;
  }
  try {
    return { event, data: JSON.parse(dataLines.join('\n')) as Record<string, unknown> };
  } catch {
    return null;
  }
}

/**
 * EventSource cannot POST and cannot carry a body, so the stream is read
 * manually off the fetch response. This also makes cancel() possible.
 */
async function* readSseFrames(response: Response): AsyncGenerator<SseFrame> {
  const reader = response.body?.getReader();
  if (reader === undefined) {
    return;
  }
  const decoder = new TextDecoder();
  let buffer = '';

  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) {
        break;
      }
      buffer += decoder.decode(value, { stream: true });

      let boundary = buffer.indexOf('\n\n');
      while (boundary !== -1) {
        const raw = buffer.slice(0, boundary);
        buffer = buffer.slice(boundary + 2);
        const frame = parseFrame(raw);
        if (frame !== null) {
          yield frame;
        }
        boundary = buffer.indexOf('\n\n');
      }
    }
  } finally {
    reader.releaseLock();
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

function buildUserTurn(text: string): HTMLElement {
  const article = document.createElement('article');
  article.className = 'turn turn-user';
  article.append(buildModeBadge(currentMode));
  const div = document.createElement('div');
  div.className = 'content';
  div.textContent = text;
  article.append(div);
  return article;
}

interface AssistantTurn {
  article: HTMLElement;
  reasoningSection: HTMLDetailsElement;
  reasoningBody: HTMLElement;
  content: HTMLElement;
  toolList: HTMLElement;
  assetList: HTMLElement;
}

function buildAssistantTurn(): AssistantTurn {
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

  article.append(buildModeBadge(currentMode), reasoningSection, toolList, content, assetList);
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

function appendStoredMessage(message: StoredMessage): void {
  const key = String(message.id);
  if (renderedMessageIds.has(key)) {
    return;
  }
  renderedMessageIds.add(key);
  stream.append(renderStoredMessage(message));
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
 * A render finishes minutes after the POST that started it, so results arrive
 * on this stream rather than on the turn stream. EventSource reconnects on its
 * own; a completion missed while offline still shows up on the next page load,
 * because the worker also writes it into the conversation.
 */
function openConversationStream(conversationId: string): void {
  closeConversationStream();
  const source = new EventSource(`/api/conversations/${conversationId}/events`);

  source.addEventListener('message_added', (event) => {
    const payload = JSON.parse((event as MessageEvent<string>).data) as {
      type: string;
      message: StoredMessage;
    };
    appendStoredMessage(payload.message);
  });

  source.addEventListener('task_updated', (event) => {
    const payload = JSON.parse((event as MessageEvent<string>).data) as {
      taskId: number;
      kind: string;
      status: string;
    };
    upsertPendingTask(payload.taskId, payload.kind, payload.status);
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
 * addressed by `data-delete-conversation`, which the delegated listener below
 * picks up.
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
  const response = await fetch('/api/conversations');
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

/**
 * The second step: a row that has been asked about swaps its delete control for
 * 确认 / 取消. In the row rather than in a browser dialog, so the answer lands
 * next to the thing it is about and the click that follows is deliberate.
 */
function askToDelete(button: HTMLButtonElement): void {
  const item = button.closest('li');
  const id = button.dataset.deleteConversation ?? '';
  if (item === null || id.length === 0) {
    return;
  }

  const strip = document.createElement('span');
  strip.className = 'conversation-confirm';

  const question = document.createElement('span');
  question.textContent = '删除？';

  const confirm = document.createElement('button');
  confirm.type = 'button';
  confirm.className = 'conversation-confirm-yes';
  confirm.dataset.confirmDelete = id;
  confirm.textContent = '确认';

  const cancel = document.createElement('button');
  cancel.type = 'button';
  cancel.className = 'conversation-confirm-no';
  cancel.dataset.cancelDelete = 'true';
  cancel.textContent = '取消';

  strip.append(question, confirm, cancel);
  button.hidden = true;
  item.append(strip);
}

/** Puts the row back. A list refresh clears it too. */
function cancelDelete(button: HTMLButtonElement): void {
  const item = button.closest('li');
  button.closest('.conversation-confirm')?.remove();
  const remove =
    item === null ? null : item.querySelector<HTMLButtonElement>('button[data-delete-conversation]');
  if (remove !== null) {
    remove.hidden = false;
  }
}

/**
 * Hides a conversation from the history. Reached only from the confirm strip, so
 * the deletion is always two clicks.
 */
async function deleteConversation(button: HTMLButtonElement): Promise<void> {
  const id = Number(button.dataset.confirmDelete ?? '');
  if (!Number.isInteger(id) || id <= 0) {
    return;
  }

  const response = await fetch(`/api/conversations/${id}/delete`, { method: 'POST' });
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

let busy = false;

async function sendPrompt(prompt: string): Promise<void> {
  if (busy) {
    return;
  }
  busy = true;
  sendButton.disabled = true;
  input.disabled = true;

  const userTurn = buildUserTurn(prompt);
  stream.append(userTurn);
  appendTurnAnchor(userTurn, prompt);
  const turn = buildAssistantTurn();
  stream.append(turn.article);
  scrollToBottom();

  let answer = '';
  let pendingTool: HTMLLIElement | null = null;

  try {
    const conversationId = body.dataset.conversationId ?? '';
    const response = await fetch('/api/chat', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        prompt,
        mode: currentMode,
        conversationId: conversationId.length > 0 ? Number(conversationId) : null,
      }),
    });

    if (!response.ok) {
      throw new Error(`请求失败（HTTP ${response.status}）`);
    }

    for await (const frame of readSseFrames(response)) {
      switch (frame.event) {
        case 'connected': {
          const id = readNumber(frame.data, 'conversationId');
          if (Number.isFinite(id)) {
            const isNew = body.dataset.conversationId !== String(id);
            body.dataset.conversationId = String(id);
            if (isNew) {
              // Keep the address bar in sync so a reload restores this conversation.
              window.history.replaceState(null, '', `${window.location.pathname}?c=${id}`);
              void refreshConversationList();
              // Long-running jobs push their results here, not on this turn stream.
              openConversationStream(String(id));
            }
          }
          break;
        }

        case 'reasoning': {
          turn.reasoningBody.append(readString(frame.data, 'delta'));
          break;
        }

        case 'text': {
          answer += readString(frame.data, 'delta');
          turn.content.innerHTML = marked.parse(answer, { async: false });
          break;
        }

        case 'tool_call': {
          const name = readString(frame.data, 'name');
          pendingTool = appendToolCall(turn.toolList, name);
          break;
        }

        case 'tool_result': {
          const summary = readString(frame.data, 'summary');
          if (pendingTool !== null) {
            const name = pendingTool.textContent?.replace(' 执行中…', '') ?? '';
            pendingTool.textContent = `${name} — ${summary}`;
            pendingTool = null;
          }
          break;
        }

        case 'asset': {
          appendAsset(
            turn.assetList,
            readString(frame.data, 'url'),
            readString(frame.data, 'kind'),
          );
          break;
        }

        case 'error': {
          const message = readString(frame.data, 'message');
          const error = document.createElement('p');
          error.className = 'turn-error';
          error.textContent = message;
          turn.article.append(error);
          break;
        }

        default:
          break;
      }
      scrollToBottom();
    }
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    const paragraph = document.createElement('p');
    paragraph.className = 'turn-error';
    paragraph.textContent = message;
    turn.article.append(paragraph);
  } finally {
    if (turn.reasoningBody.textContent === '') {
      turn.reasoningSection.remove();
    } else {
      turn.reasoningSection.open = false;
    }
    if (turn.content.textContent === '') {
      turn.content.remove();
    }
    busy = false;
    sendButton.disabled = false;
    input.disabled = false;
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

  const confirmed = target.closest('button[data-confirm-delete]');
  if (confirmed instanceof HTMLButtonElement) {
    void deleteConversation(confirmed);
    return;
  }

  const cancelled = target.closest('button[data-cancel-delete]');
  if (cancelled instanceof HTMLButtonElement) {
    cancelDelete(cancelled);
    return;
  }

  const asked = target.closest('button[data-delete-conversation]');
  if (asked instanceof HTMLButtonElement) {
    askToDelete(asked);
  }
});

/** Back to the "no conversation yet" state, for 新会话 and for a deleted one. */
function resetToNewConversation(): void {
  delete body.dataset.conversationId;
  stream.replaceChildren();
  closeConversationStream();
  renderedMessageIds.clear();
  clearTurnAnchors();
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
  openConversationStream(initialConversationId);
}

input.focus();
