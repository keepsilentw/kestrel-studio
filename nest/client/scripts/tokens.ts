/**
 * The device-token screen. It is deliberately outside both browser bundles:
 * /settings/tokens is a small, frameworkless surface that talks to the same
 * JSON endpoint native hosts use, so the token logic has one implementation.
 */

const TOKEN_ENDPOINT = '/api/v1/tokens';

function requireElement<T extends Element>(selector: string): T {
  const element = document.querySelector<T>(selector);
  if (element === null) {
    throw new Error(`Missing element: ${selector}`);
  }
  return element;
}

interface TokenSummary {
  id: number;
  label: string;
  createdAt: number;
  lastSeenAt: number | null;
}

interface IssuedToken {
  id: number;
  label: string;
  token: string;
  createdAt: number;
}

type RequestResult =
  | { ok: true; value: unknown }
  | { ok: false; message: string };

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null;
}

function isTokenSummary(value: unknown): value is TokenSummary {
  return (
    isRecord(value) &&
    typeof value.id === 'number' &&
    typeof value.label === 'string' &&
    typeof value.createdAt === 'number' &&
    (value.lastSeenAt === null || typeof value.lastSeenAt === 'number')
  );
}

function isIssuedToken(value: unknown): value is IssuedToken {
  return (
    isRecord(value) &&
    typeof value.id === 'number' &&
    typeof value.label === 'string' &&
    typeof value.token === 'string' &&
    typeof value.createdAt === 'number'
  );
}

async function readJson(response: Response): Promise<unknown> {
  try {
    return await response.json();
  } catch {
    return null;
  }
}

async function request(path: string, init: RequestInit): Promise<RequestResult> {
  try {
    const response = await fetch(path, { credentials: 'same-origin', ...init });
    const body = await readJson(response);
    if (!response.ok) {
      const message =
        isRecord(body) && typeof body.message === 'string'
          ? body.message
          : `请求失败（${response.status}）`;
      return { ok: false, message };
    }
    return { ok: true, value: body };
  } catch {
    return { ok: false, message: '网络异常，请稍后重试' };
  }
}

const form = requireElement<HTMLFormElement>('#token-create-form');
const labelInput = requireElement<HTMLInputElement>('#token-label');
const notice = requireElement<HTMLElement>('#token-notice');
const reveal = requireElement<HTMLElement>('#token-reveal');
const tokenValue = requireElement<HTMLElement>('#token-value');
const copyButton = requireElement<HTMLButtonElement>('#token-copy');
const count = requireElement<HTMLElement>('#token-count');
const list = requireElement<HTMLElement>('#token-list');
const revokeDialog = requireElement<HTMLDialogElement>('#token-revoke-dialog');
const revokeTarget = requireElement<HTMLElement>('#token-revoke-target');

const timeFormat = new Intl.DateTimeFormat('zh-CN', {
  year: 'numeric',
  month: '2-digit',
  day: '2-digit',
  hour: '2-digit',
  minute: '2-digit',
  hour12: false,
});

let pendingRevokeId: number | null = null;
let creating = false;
let revoking = false;

function formatTime(ms: number): string {
  const date = new Date(ms);
  return Number.isNaN(date.getTime()) ? '—' : timeFormat.format(date);
}

function showNotice(text: string, isError = false): void {
  notice.textContent = text;
  notice.classList.toggle('admin-notice-error', isError);
  notice.hidden = false;
}

function emptyRow(text: string): HTMLTableRowElement {
  const row = document.createElement('tr');
  const cell = document.createElement('td');
  cell.colSpan = 4;
  cell.className = 'admin-empty';
  cell.textContent = text;
  row.append(cell);
  return row;
}

function tokenRow(token: TokenSummary): HTMLTableRowElement {
  const row = document.createElement('tr');

  const label = document.createElement('td');
  label.textContent = token.label;

  const created = document.createElement('td');
  created.textContent = formatTime(token.createdAt);

  const seen = document.createElement('td');
  seen.textContent =
    token.lastSeenAt === null ? '从未使用' : formatTime(token.lastSeenAt);

  const action = document.createElement('td');
  const revoke = document.createElement('button');
  revoke.type = 'button';
  revoke.className = 'admin-button admin-button-quiet';
  revoke.textContent = '撤销';
  revoke.dataset.tokenId = String(token.id);
  revoke.dataset.tokenLabel = token.label;
  action.append(revoke);

  row.append(label, created, seen, action);
  return row;
}

function paint(tokens: TokenSummary[]): void {
  count.textContent = String(tokens.length);
  list.replaceChildren();
  if (tokens.length === 0) {
    list.append(emptyRow('还没有设备令牌'));
    return;
  }
  for (const token of tokens) {
    list.append(tokenRow(token));
  }
}

async function refresh(): Promise<void> {
  const result = await request(TOKEN_ENDPOINT, { method: 'GET' });
  if (!result.ok) {
    list.replaceChildren(emptyRow(result.message));
    count.textContent = '0';
    showNotice(result.message, true);
    return;
  }
  if (!Array.isArray(result.value) || !result.value.every(isTokenSummary)) {
    showNotice('令牌列表格式不正确', true);
    return;
  }
  paint(result.value);
}

form.addEventListener('submit', (event) => {
  event.preventDefault();
  void createToken();
});

async function createToken(): Promise<void> {
  if (creating) {
    return;
  }
  creating = true;
  const result = await request(TOKEN_ENDPOINT, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ label: labelInput.value }),
  });
  creating = false;
  if (!result.ok) {
    showNotice(result.message, true);
    return;
  }
  if (!isIssuedToken(result.value)) {
    showNotice('签发结果格式不正确', true);
    return;
  }
  tokenValue.textContent = result.value.token;
  reveal.hidden = false;
  labelInput.value = '';
  showNotice('令牌已签发。请现在复制，离开页面后无法再次查看。');
  await refresh();
}

copyButton.addEventListener('click', () => {
  void copyToken();
});

async function copyToken(): Promise<void> {
  const text = tokenValue.textContent ?? '';
  if (text.length === 0) {
    showNotice('没有可复制的令牌', true);
    return;
  }
  try {
    await navigator.clipboard.writeText(text);
    showNotice('已复制到剪贴板');
  } catch {
    showNotice('复制失败，请手动选择令牌文本', true);
  }
}

list.addEventListener('click', (event) => {
  const target = event.target;
  if (!(target instanceof HTMLElement)) {
    return;
  }
  const button = target.closest<HTMLButtonElement>('[data-token-id]');
  if (button === null) {
    return;
  }
  const id = Number(button.dataset.tokenId);
  if (!Number.isInteger(id) || id <= 0) {
    return;
  }
  pendingRevokeId = id;
  revokeTarget.textContent = button.dataset.tokenLabel ?? '';
  if (!revokeDialog.open) {
    revokeDialog.showModal();
  }
});

revokeDialog.addEventListener('close', () => {
  pendingRevokeId = null;
});

for (const button of revokeDialog.querySelectorAll<HTMLButtonElement>(
  '[data-token-revoke-choice]',
)) {
  button.addEventListener('click', () => {
    if (button.dataset.tokenRevokeChoice === 'confirm') {
      void confirmRevoke();
    } else {
      revokeDialog.close();
    }
  });
}

async function confirmRevoke(): Promise<void> {
  if (revoking || pendingRevokeId === null) {
    return;
  }
  const id = pendingRevokeId;
  revoking = true;
  const result = await request(`${TOKEN_ENDPOINT}/${id}`, { method: 'DELETE' });
  revoking = false;
  revokeDialog.close();
  if (!result.ok) {
    showNotice(result.message, true);
    return;
  }
  showNotice('令牌已撤销，下一次请求会返回 401');
  await refresh();
}

void refresh();
