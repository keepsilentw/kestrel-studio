import { Injectable, Logger } from '@nestjs/common';
import { mkdir, writeFile } from 'node:fs/promises';
import { extname, join, resolve } from 'node:path';
import { resolveApiKey } from '@/bailian/token';
import { loadConfig } from '@/config/configuration';

export const DEFAULT_IMAGE_MODEL = 'wan2.7-image';

const IMAGE_MODELS = new Set(['wan2.7-image', 'wan2.7-image-pro']);

const MIME_BY_EXTENSION: Record<string, string> = {
  '.png': 'image/png',
  '.jpg': 'image/jpeg',
  '.jpeg': 'image/jpeg',
  '.webp': 'image/webp',
  '.mp4': 'video/mp4',
  '.mov': 'video/quicktime',
  '.webm': 'video/webm',
};

const DEFAULT_EXTENSION: Record<AssetKind, string> = {
  image: '.png',
  video: '.mp4',
};

export type AssetKind = 'image' | 'video';

export interface StoredAsset {
  kind: AssetKind;
  /**
   * Where the bytes were written, absolute. Persisted as its base name only —
   * see `storedAssetName` in `asset-path.ts` for why.
   */
  filePath: string;
  sourceUrl: string;
  mime: string;
  bytes: number;
}

export function assertImageModel(model: string): void {
  if (!IMAGE_MODELS.has(model)) {
    throw new Error(`Unsupported image model "${model}". Supported: ${[...IMAGE_MODELS].join(', ')}`);
  }
}

interface ImageResponse {
  output?: {
    choices?: { message?: { content?: { type?: string; image?: string }[] } }[];
  };
}

/** Normalized provider task state. Everything unrecognized counts as terminal failure. */
export type VideoTaskState = 'pending' | 'running' | 'succeeded' | 'failed';

export interface VideoTaskSnapshot {
  state: VideoTaskState;
  /** The provider's own status string, kept for messages and diagnostics. */
  rawStatus: string;
  videoUrl: string | null;
  message: string | null;
}

interface VideoSubmitResponse {
  output?: { task_id?: string; task_status?: string };
}

interface VideoQueryResponse {
  output?: {
    task_id?: string;
    task_status?: string;
    video_url?: string;
    code?: string;
    message?: string;
  };
}

/** Exported for tests: the SUCCEEDED-without-URL rule is worth pinning. */
export function normalizeTaskState(rawStatus: string, hasVideo: boolean): VideoTaskState {
  switch (rawStatus) {
    case 'PENDING':
      return 'pending';
    case 'RUNNING':
      return 'running';
    case 'SUCCEEDED':
      // A SUCCEEDED task without a URL has nothing to hand back; treat it as a failure
      // rather than polling it forever.
      return hasVideo ? 'succeeded' : 'failed';
    default:
      return 'failed';
  }
}

@Injectable()
export class MediaService {
  private readonly logger = new Logger(MediaService.name);

  /**
   * Calls the multimodal-generation endpoint, which returns image URLs.
   * Same path and payload shape as playground/bailian-media-mcp.
   */
  async generateImages(options: {
    prompt: string;
    model: string;
    size: string;
    count: number;
    signal?: AbortSignal;
  }): Promise<string[]> {
    assertImageModel(options.model);
    const { bailian } = loadConfig();

    const response = await fetch(
      `${bailian.baseUrl}/api/v1/services/aigc/multimodal-generation/generation`,
      {
        method: 'POST',
        headers: {
          Authorization: `Bearer ${resolveApiKey()}`,
          'Content-Type': 'application/json',
        },
        body: JSON.stringify({
          model: options.model,
          input: { messages: [{ role: 'user', content: [{ text: options.prompt }] }] },
          parameters: { size: options.size, n: options.count },
        }),
        ...(options.signal !== undefined ? { signal: options.signal } : {}),
      },
    );

    const raw = await response.text();
    if (!response.ok) {
      throw new Error(`Image generation failed (HTTP ${response.status}): ${raw.slice(0, 300)}`);
    }

    let payload: ImageResponse;
    try {
      payload = JSON.parse(raw) as ImageResponse;
    } catch {
      throw new Error(`Bailian returned non-JSON image response: ${raw.slice(0, 200)}`);
    }

    const content = payload.output?.choices?.[0]?.message?.content ?? [];
    const urls = content
      .filter((part) => part.type === 'image' && typeof part.image === 'string')
      .map((part) => part.image as string);

    if (urls.length === 0) {
      throw new Error('Bailian returned no image for this request.');
    }
    return urls;
  }

  /**
   * Submits an async video job and returns the provider task id. The submit call
   * only accepts the job; the render itself is collected by `queryVideoTask`.
   */
  async submitVideo(options: {
    prompt: string;
    model: string;
    size: string;
    duration: number;
    firstFrameUrl?: string;
  }): Promise<string> {
    const { bailian } = loadConfig();
    const media =
      options.firstFrameUrl !== undefined
        ? [{ type: 'first_frame', url: options.firstFrameUrl }]
        : [];

    const response = await fetch(
      `${bailian.baseUrl}/api/v1/services/aigc/video-generation/video-synthesis`,
      {
        method: 'POST',
        headers: {
          Authorization: `Bearer ${resolveApiKey()}`,
          'Content-Type': 'application/json',
          // Without this header the endpoint runs synchronously and the request
          // would hang for the whole render.
          'X-DashScope-Async': 'enable',
        },
        body: JSON.stringify({
          model: options.model,
          input: {
            prompt: options.prompt,
            ...(media.length > 0 ? { media } : {}),
          },
          parameters: { size: options.size, duration: options.duration },
        }),
      },
    );

    const raw = await response.text();
    if (!response.ok) {
      throw new Error(`Video submission failed (HTTP ${response.status}): ${raw.slice(0, 300)}`);
    }

    let payload: VideoSubmitResponse;
    try {
      payload = JSON.parse(raw) as VideoSubmitResponse;
    } catch {
      throw new Error(`Bailian returned non-JSON video response: ${raw.slice(0, 200)}`);
    }

    const taskId = payload.output?.task_id;
    if (typeof taskId !== 'string' || taskId.length === 0) {
      throw new Error('Bailian did not return a video task id.');
    }
    return taskId;
  }

  /** One poll of `/api/v1/tasks/{id}`. Callers own the retry/backoff policy. */
  async queryVideoTask(providerTaskId: string): Promise<VideoTaskSnapshot> {
    const { bailian } = loadConfig();
    const response = await fetch(
      `${bailian.baseUrl}/api/v1/tasks/${encodeURIComponent(providerTaskId)}`,
      { headers: { Authorization: `Bearer ${resolveApiKey()}` } },
    );

    const raw = await response.text();
    if (!response.ok) {
      throw new Error(`Video task query failed (HTTP ${response.status}): ${raw.slice(0, 300)}`);
    }

    let payload: VideoQueryResponse;
    try {
      payload = JSON.parse(raw) as VideoQueryResponse;
    } catch {
      throw new Error(`Bailian returned non-JSON task response: ${raw.slice(0, 200)}`);
    }

    const output = payload.output ?? {};
    const videoUrl = typeof output.video_url === 'string' && output.video_url.length > 0
      ? output.video_url
      : null;
    const rawStatus = typeof output.task_status === 'string' ? output.task_status : 'UNKNOWN';
    const message = typeof output.message === 'string' && output.message.length > 0
      ? output.message
      : (typeof output.code === 'string' && output.code.length > 0 ? output.code : null);

    return {
      state: normalizeTaskState(rawStatus, videoUrl !== null),
      rawStatus,
      videoUrl,
      message,
    };
  }

  /** Downloads a generated asset into the storage directory. */
  async store(kind: AssetKind, sourceUrl: string): Promise<StoredAsset> {
    const response = await fetch(sourceUrl);
    if (!response.ok) {
      throw new Error(`Failed to download generated ${kind} (HTTP ${response.status}).`);
    }
    const bytes = Buffer.from(await response.arrayBuffer());
    const directory = resolve(loadConfig().storageDir);
    await mkdir(directory, { recursive: true });

    const extension = this.resolveExtension(kind, sourceUrl);
    const stamp = new Date().toISOString().replace(/[:.]/g, '-');
    const suffix = Math.random().toString(36).slice(2, 8);
    const filePath = join(directory, `${kind}-${stamp}-${suffix}${extension}`);
    await writeFile(filePath, bytes);

    return {
      kind,
      filePath,
      sourceUrl,
      mime: MIME_BY_EXTENSION[extension] ?? 'application/octet-stream',
      bytes: bytes.byteLength,
    };
  }

  private resolveExtension(kind: AssetKind, sourceUrl: string): string {
    const allowed = kind === 'video' ? ['.mp4', '.mov', '.webm'] : ['.png', '.jpg', '.jpeg', '.webp'];
    let fromUrl = '';
    try {
      fromUrl = extname(new URL(sourceUrl).pathname).toLowerCase();
    } catch {
      fromUrl = '';
    }
    return allowed.includes(fromUrl) ? fromUrl : DEFAULT_EXTENSION[kind];
  }
}
