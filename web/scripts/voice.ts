import '../styles/main.css';

/**
 * The /voice page: hold the button, speak, hear the answer.
 *
 * The shape of every frame here mirrors `src/voice/protocol.ts`, which is the
 * server-side source of truth. The two are restated by hand because this file
 * cannot import from `src/` — the `@` alias and Node globals would come with it.
 * Keep them in step.
 *
 * Audio is PCM16 mono at 24kHz in both directions. The upstream contract is
 * verified in docs/verification.md §3: `input_audio_buffer.append` takes raw
 * PCM (a WAV header would be played as audio and garble the utterance), and the
 * reply arrives the same way.
 */

const SAMPLE_RATE = 24000;

/** ~43ms per message: small enough for latency, large enough not to flood the socket. */
const SAMPLES_PER_MESSAGE = 1024;

const NOTE_RECONNECT = '会话已结束，再按一次继续';
const NOTE_NO_MIC = '拿不到麦克风权限，无法说话';
const NOTE_HOLD_LIMIT = '已到单段上限，这段先提交了；还有话要说请松开再按一次';

/**
 * The endpoint rejects anything past 30s of *uncommitted* audio:
 *
 *   Input audio buffer exceeded maximum duration (30s). Please commit or clear
 *   the buffer.
 *
 * Once it starts rejecting it rejects every appended chunk — ~23 errors a second
 * at this batching — which is what a long hold looks like from the page. So the
 * hold is cut short of the limit: commit what there is, stop feeding, and let the
 * user press again for more.
 */
const MAX_HOLD_MS = 25_000;

function requireElement<T extends Element>(selector: string): T {
  const element = document.querySelector<T>(selector);
  if (element === null) {
    throw new Error(`Missing element: ${selector}`);
  }
  return element;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null;
}

function readString(source: Record<string, unknown>, key: string): string {
  const value = source[key];
  return typeof value === 'string' ? value : '';
}

function readNumber(source: Record<string, unknown>, key: string): number {
  const value = source[key];
  return typeof value === 'number' ? value : Number.NaN;
}

const log = requireElement<HTMLOListElement>('#voice-log');
const status = requireElement<HTMLParagraphElement>('#voice-status');
const button = requireElement<HTMLButtonElement>('#voice-button');

/* ---------- base64 ---------- */

function bytesToBase64(bytes: Uint8Array): string {
  let binary = '';
  // Chunked: spreading a whole recording into fromCharCode overflows the
  // argument limit.
  const CHUNK = 0x8000;
  for (let offset = 0; offset < bytes.length; offset += CHUNK) {
    binary += String.fromCharCode(...bytes.subarray(offset, offset + CHUNK));
  }
  return btoa(binary);
}

function base64ToBytes(base64: string): Uint8Array {
  const binary = atob(base64);
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) {
    bytes[index] = binary.charCodeAt(index);
  }
  return bytes;
}

/* ---------- log ---------- */

function appendEntry(kind: string, text: string): HTMLElement {
  const entry = document.createElement('li');
  entry.className = `voice-entry voice-entry-${kind}`;
  entry.textContent = text;
  log.append(entry);
  log.scrollTop = log.scrollHeight;
  return entry;
}

function appendAsset(kind: string, url: string): void {
  const entry = document.createElement('li');
  entry.className = 'voice-entry voice-entry-assistant';

  const figure = document.createElement('figure');
  figure.className = 'asset';

  const media = document.createElement(kind === 'video' ? 'video' : 'img');
  media.className = kind === 'video' ? 'asset-video' : 'asset-image';
  media.setAttribute('src', url);
  if (media instanceof HTMLVideoElement) {
    media.controls = true;
    media.preload = 'metadata';
  } else {
    media.setAttribute('alt', '生成的图片');
    media.setAttribute('loading', 'lazy');
  }

  const caption = document.createElement('figcaption');
  caption.className = 'asset-actions';
  const link = document.createElement('a');
  link.className = 'asset-download';
  link.setAttribute('href', url);
  link.setAttribute('download', '');
  link.textContent = '下载';
  caption.append(link);

  figure.append(media, caption);
  entry.append(figure);
  log.append(entry);
  log.scrollTop = log.scrollHeight;
}

function setStatus(text: string): void {
  status.textContent = text;
}

/* ---------- capture ---------- */

/**
 * Batches 128-frame quanta into ~43ms messages.
 *
 * `channelCount: 1` with an explicit mode is load-bearing: an AudioWorkletNode
 * defaults to stereo, and stereo PCM16 sent to a mono contract would be wrong
 * without being rejected. `numberOfOutputs: 0` keeps the mic out of the
 * speakers, so there is no feedback path.
 */
const CAPTURE_WORKLET = `
class Capture extends AudioWorkletProcessor {
  constructor() {
    super();
    this.frame = new Int16Array(${SAMPLES_PER_MESSAGE});
    this.filled = 0;
  }

  process(inputs) {
    const input = inputs[0];
    const channel = input ? input[0] : null;
    if (!channel) {
      return true;
    }
    for (let i = 0; i < channel.length; i += 1) {
      const sample = Math.max(-1, Math.min(1, channel[i]));
      this.frame[this.filled] = sample < 0 ? sample * 0x8000 : sample * 0x7fff;
      this.filled += 1;
      if (this.filled === this.frame.length) {
        this.port.postMessage(this.frame.buffer, [this.frame.buffer]);
        this.frame = new Int16Array(${SAMPLES_PER_MESSAGE});
        this.filled = 0;
      }
    }
    return true;
  }
}
registerProcessor('capture', Capture);
`;

interface Capture {
  context: AudioContext;
  stream: MediaStream;
  source: MediaStreamAudioSourceNode;
  node: AudioWorkletNode;
}

let capture: Capture | null = null;
let playHead = 0;

async function openAudio(): Promise<AudioContext> {
  // A non-default rate is honoured exactly by Chrome, which is what lets the
  // browser resample the microphone for us — no hand-written resampler.
  const context = new AudioContext({ sampleRate: SAMPLE_RATE });
  if (context.sampleRate !== SAMPLE_RATE) {
    // Not expected on Chrome; surfaced because the alternative is silent
    // pitch-shifted audio rather than an error.
    appendEntry('note', `浏览器把采样率改为 ${context.sampleRate}Hz，音质可能异常`);
  }
  await context.resume();
  return context;
}

async function startCapture(context: AudioContext): Promise<Capture> {
  const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
  const url = URL.createObjectURL(
    new Blob([CAPTURE_WORKLET], { type: 'text/javascript' }),
  );
  await context.audioWorklet.addModule(url);
  URL.revokeObjectURL(url);

  const source = context.createMediaStreamSource(stream);
  const node = new AudioWorkletNode(context, 'capture', {
    numberOfInputs: 1,
    numberOfOutputs: 0,
    channelCount: 1,
    channelCountMode: 'explicit',
  });
  source.connect(node);
  return { context, stream, source, node };
}

function stopCapture(current: Capture): void {
  current.node.port.onmessage = null;
  current.source.disconnect();
  current.node.disconnect();
  for (const track of current.stream.getTracks()) {
    track.stop();
  }
}

function playChunk(base64: string): void {
  if (capture === null) {
    return;
  }
  const bytes = base64ToBytes(base64);
  const samples = new Int16Array(bytes.buffer, bytes.byteOffset, Math.floor(bytes.byteLength / 2));
  const buffer = capture.context.createBuffer(1, samples.length, SAMPLE_RATE);
  const channel = buffer.getChannelData(0);
  for (let index = 0; index < samples.length; index += 1) {
    channel[index] = samples[index] / 0x8000;
  }

  const source = capture.context.createBufferSource();
  source.buffer = buffer;
  source.connect(capture.context.destination);
  // Queued end to end rather than started on arrival, so consecutive deltas do
  // not overlap or leave gaps.
  playHead = Math.max(playHead, capture.context.currentTime);
  source.start(playHead);
  playHead += buffer.duration;
}

/* ---------- socket ---------- */

let socket: WebSocket | null = null;
let ready = false;
let busy = false;
let pending: string[] = [];
let pendingCommit = false;
let conversationId: number | null = null;
let events: EventSource | null = null;
/** This utterance was already committed, or the endpoint refused it. */
let blocked = false;
let holdTimer: number | null = null;

async function ensureSocket(): Promise<WebSocket> {
  if (socket !== null) {
    const state = socket.readyState;
    // Reuse a socket that is still coming up: opening a second one would leave
    // the first session running on the server with nobody listening to it.
    if (state === WebSocket.OPEN || state === WebSocket.CONNECTING) {
      return socket;
    }
  }

  // The session cookie cannot authorize the handshake — an upgrade never
  // reaches Express — so it is traded for a single-use ticket first.
  const response = await fetch('/api/voice/ticket', { method: 'POST' });
  if (!response.ok) {
    throw new Error(`无法开始语音会话（HTTP ${response.status}）`);
  }
  const issued: unknown = await response.json();
  const ticket = isRecord(issued) ? readString(issued, 'ticket') : '';
  if (ticket.length === 0) {
    throw new Error('服务器没有签发语音票据');
  }

  const scheme = window.location.protocol === 'https:' ? 'wss' : 'ws';
  const opened = new WebSocket(
    `${scheme}://${window.location.host}/api/voice?ticket=${encodeURIComponent(ticket)}`,
  );
  opened.onopen = () => {
    // This is the frame that opens the call. The server does nothing at all
    // until it arrives — no upstream session, no `ready` — so without it the
    // page sits on "正在连接…" with a perfectly healthy socket.
    send({ type: 'start' });
  };
  opened.onmessage = (event: MessageEvent<string>) => {
    handleFrame(event.data);
  };
  opened.onclose = () => {
    socket = null;
    ready = false;
    closeEventStream();
    if (conversationId !== null) {
      appendEntry('note', NOTE_RECONNECT);
      setStatus('会话已结束');
    }
  };
  socket = opened;
  return opened;
}

function send(payload: Record<string, unknown>): void {
  if (socket !== null && socket.readyState === WebSocket.OPEN) {
    socket.send(JSON.stringify(payload));
  }
}

function handleFrame(raw: string): void {
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return;
  }
  if (!isRecord(parsed)) {
    return;
  }

  switch (readString(parsed, 'type')) {
    case 'ready': {
      ready = true;
      const id = readNumber(parsed, 'conversationId');
      if (Number.isFinite(id)) {
        conversationId = id;
        openEventStream(id);
      }
      for (const chunk of pending) {
        send({ type: 'audio', pcm: chunk });
      }
      pending = [];
      if (pendingCommit) {
        pendingCommit = false;
        send({ type: 'commit' });
      }
      // The status was written when the button went down, before there was a
      // session to listen to; now there is one.
      if (holding) {
        setStatus('正在听…');
        armHoldLimit();
      }
      break;
    }

    case 'transcript': {
      appendEntry('user', readString(parsed, 'text'));
      setStatus('正在思考…');
      break;
    }

    case 'tool_call': {
      const name = readString(parsed, 'name');
      const state = readString(parsed, 'status');
      appendEntry('tool', state === 'running' ? `${name} 执行中…` : `${name} 已完成`);
      break;
    }

    case 'asset': {
      appendAsset(readString(parsed, 'kind'), readString(parsed, 'url'));
      break;
    }

    case 'assistant_text': {
      appendEntry('assistant', readString(parsed, 'text'));
      break;
    }

    case 'audio': {
      setStatus('正在回答…');
      playChunk(readString(parsed, 'pcm'));
      break;
    }

    case 'done': {
      busy = false;
      button.disabled = false;
      setStatus('按住按钮开始说话');
      break;
    }

    case 'error': {
      appendEntry('error', readString(parsed, 'message'));
      // Whatever the endpoint refused, more of the same will not help: stop
      // feeding it for the rest of this hold.
      blocked = true;
      busy = false;
      button.disabled = false;
      break;
    }

    default:
      break;
  }
}

/**
 * Long renders finish after the call is over: a video submitted by voice lands
 * as a new message and is pushed on the conversation stream, not the socket.
 * Images do not come this way — the turn stream carries them, and a call has no
 * turn stream, which is why they are sent inline above.
 */
function openEventStream(id: number): void {
  events?.close();
  const source = new EventSource(`/api/conversations/${id}/events`);
  source.addEventListener('message_added', (event) => {
    const payload: unknown = JSON.parse((event as MessageEvent<string>).data);
    if (!isRecord(payload)) {
      return;
    }
    const message = payload.message;
    if (!isRecord(message)) {
      return;
    }
    const text = readString(message, 'content');
    if (text.length > 0) {
      appendEntry('assistant', text);
    }
    const assets = message.assets;
    if (Array.isArray(assets)) {
      for (const asset of assets) {
        if (isRecord(asset)) {
          appendAsset(readString(asset, 'kind'), readString(asset, 'url'));
        }
      }
    }
  });
  events = source;
}

function closeEventStream(): void {
  events?.close();
  events = null;
}

/* ---------- push to talk ---------- */

let holding = false;

function clearHoldTimer(): void {
  if (holdTimer !== null) {
    window.clearTimeout(holdTimer);
    holdTimer = null;
  }
}

/**
 * Starts counting toward the buffer limit. Armed when audio actually starts
 * flowing — the queued audio is only flushed once the session is up, so the
 * endpoint's buffer starts filling then, not when the button went down.
 */
function armHoldLimit(): void {
  clearHoldTimer();
  holdTimer = window.setTimeout(endHoldAtLimit, MAX_HOLD_MS);
}

/**
 * The hold ran into the endpoint's buffer limit: hand over what has been said
 * and stop feeding. The user is told, because the alternative is the audio after
 * this point going nowhere.
 */
function endHoldAtLimit(): void {
  holdTimer = null;
  if (!holding || blocked) {
    return;
  }
  blocked = true;
  busy = true;
  send({ type: 'commit' });
  appendEntry('note', NOTE_HOLD_LIMIT);
  setStatus('正在思考…');
}

async function press(): Promise<void> {
  if (busy || holding) {
    return;
  }
  holding = true;
  blocked = false;
  button.classList.add('voice-button-active');
  setStatus('正在听…');

  try {
    if (capture === null) {
      const context = await openAudio();
      capture = await startCapture(context);
      capture.node.port.onmessage = (event: MessageEvent<ArrayBuffer>) => {
        if (blocked) {
          return;
        }
        const pcm = bytesToBase64(new Uint8Array(event.data));
        if (ready) {
          send({ type: 'audio', pcm });
        } else {
          // The first press has to open the session; nothing can be sent until
          // the upstream has accepted `session.update`.
          pending.push(pcm);
        }
      };
    }
    await ensureSocket();
    if (ready) {
      setStatus('正在听…');
      armHoldLimit();
    } else {
      setStatus('正在连接…');
    }
  } catch (error) {
    appendEntry('error', error instanceof Error ? error.message : String(error));
    release();
    busy = false;
    button.disabled = false;
    setStatus('按住按钮开始说话');
  }
}

function release(): void {
  if (!holding) {
    return;
  }
  holding = false;
  clearHoldTimer();
  button.classList.remove('voice-button-active');
  if (busy) {
    // Already handed over by the hold limit, or a turn is running.
    return;
  }
  if (blocked) {
    // The endpoint refused this utterance; committing again would submit an
    // empty buffer on top of it.
    setStatus('按住按钮开始说话');
    return;
  }
  busy = true;
  if (ready) {
    send({ type: 'commit' });
    setStatus('正在思考…');
  } else {
    // Still connecting: commit as soon as the session is up, so a quick first
    // press is not swallowed.
    pendingCommit = true;
    setStatus('正在连接…');
  }
}

button.addEventListener('pointerdown', (event) => {
  event.preventDefault();
  void press();
});

for (const type of ['pointerup', 'pointercancel', 'pointerleave']) {
  button.addEventListener(type, () => {
    release();
  });
}

window.addEventListener('pagehide', () => {
  if (capture !== null) {
    stopCapture(capture);
    capture = null;
  }
  send({ type: 'stop' });
  socket?.close();
  closeEventStream();
});

/**
 * Nothing is created until the first press: `getUserMedia` and a resumed
 * AudioContext both require a user gesture, and opening a upstream session for
 * a page that is merely being looked at would burn one.
 */
const micAvailable = navigator.mediaDevices !== undefined;
if (!micAvailable) {
  button.disabled = true;
  appendEntry('error', NOTE_NO_MIC);
}
