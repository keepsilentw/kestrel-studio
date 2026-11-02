import { Injectable } from '@nestjs/common';
import type { ResponsesFunctionTool } from '@/bailian/responses-client';
import { clamp, readNumber, readOptionalNumber, readString } from '@/common/args';
import { describeError } from '@/common/errors';
import { buildSignedFrameUrl, isPubliclyReachable } from '@/common/signed-url';
import { loadConfig } from '@/config/configuration';
import { ConversationService } from '@/conversation/conversation.service';
import { DEFAULT_IMAGE_MODEL, MediaService, type StoredAsset } from '@/media/media.service';
import { TaskService } from '@/task/task.service';
import { TOOL_NAMES_BY_MODE, type Mode } from './mode';

/**
 * Text handed back to the model as a tool result, with the ids of any assets it
 * just produced appended.
 *
 * The ids are what let a later call name a specific image — image-to-video takes
 * one as its first frame. Shared by the text turn loop and the voice session.
 */
export function describeOutcome(outcome: ToolOutcome, assetIds: number[]): string {
  if (assetIds.length === 0) {
    return outcome.output;
  }
  return `${outcome.output} Asset ids for later reference: ${assetIds.join(', ')}.`;
}

export interface ToolOutcome {
  ok: boolean;
  /** Text fed back to the model as the tool result. */
  output: string;
  /** Assets produced by this call, to be persisted and pushed to the browser. */
  assets: StoredAsset[];
}

export interface ToolContext {
  conversationId: number;
  /** Assistant placeholder of the current turn; async tasks link back to it. */
  messageId: number;
  signal?: AbortSignal;
}

export interface AgentTool {
  spec: ResponsesFunctionTool;
  execute(args: Record<string, unknown>, context: ToolContext): Promise<ToolOutcome>;
}

/**
 * A single image generation call leaves the stream silent for tens of seconds.
 * The timeout aborts the tool rather than the connection, so the failure comes
 * back to the model as a tool result it can react to.
 */
const TOOL_TIMEOUT_MS = 120_000;

const DEFAULT_VIDEO_SIZE = '1280*720';
const DEFAULT_VIDEO_DURATION = 5;
const MIN_VIDEO_DURATION = 1;
const MAX_VIDEO_DURATION = 10;

const GENERATE_IMAGE_SPEC: ResponsesFunctionTool = {
  type: 'function',
  name: 'generate_image',
  description:
    'Generate one or more images from a text prompt and save them locally. ' +
    'Use this whenever the user asks for a picture, illustration, poster, icon or any other bitmap asset.',
  parameters: {
    type: 'object',
    properties: {
      prompt: {
        type: 'string',
        description:
          'Text description of the desired image. Write it in the same language as the user request, ' +
          'and make it visually specific (subject, style, lighting, composition).',
      },
      size: {
        type: 'string',
        description: 'Output size such as 1024*1024. Defaults to 1024*1024.',
      },
      count: {
        type: 'integer',
        description: 'How many images to generate, 1 to 4. Defaults to 1.',
      },
    },
    required: ['prompt'],
    additionalProperties: false,
  },
};

const GENERATE_VIDEO_SPEC: ResponsesFunctionTool = {
  type: 'function',
  name: 'generate_video',
  description:
    'Submit a text-to-video (or image-to-video) render job. Returns immediately with a task id: ' +
    'the render takes minutes and the finished video is pushed to the page automatically, so do not ' +
    'submit the same job twice and do not poll for it.',
  parameters: {
    type: 'object',
    properties: {
      prompt: {
        type: 'string',
        description:
          'What should happen in the video: subject, action, camera movement, lighting, mood. ' +
          'Write it in the same language as the user request.',
      },
      size: {
        type: 'string',
        description: 'Output size such as 1280*720. Defaults to 1280*720.',
      },
      duration: {
        type: 'integer',
        description: `Clip length in seconds, ${MIN_VIDEO_DURATION} to ${MAX_VIDEO_DURATION}. Defaults to ${DEFAULT_VIDEO_DURATION}.`,
      },
      first_frame_asset_id: {
        type: 'integer',
        description:
          'Animate an existing image from this conversation by using it as the first frame. ' +
          'Pass the asset id that generate_image reported for that image. ' +
          'Omit this parameter for a normal text-to-video render.',
      },
    },
    required: ['prompt'],
    additionalProperties: false,
  },
};

const GET_VIDEO_TASK_SPEC: ResponsesFunctionTool = {
  type: 'function',
  name: 'get_video_task',
  description:
    'Check the status of a video task submitted earlier in this conversation. ' +
    'Only call this when the user asks about progress; finished videos arrive on their own.',
  parameters: {
    type: 'object',
    properties: {
      task_id: {
        type: 'integer',
        description: 'The task id returned by generate_video.',
      },
    },
    required: ['task_id'],
    additionalProperties: false,
  },
};

const TASK_STATUS_TEXT: Record<string, string> = {
  queued: '排队中',
  running: '渲染中',
  succeeded: '已完成',
  failed: '失败',
};

function createGenerateImageTool(media: MediaService): AgentTool {
  return {
    spec: GENERATE_IMAGE_SPEC,
    async execute(args, context): Promise<ToolOutcome> {
      const prompt = readString(args, 'prompt', '');
      if (prompt.length === 0) {
        return { ok: false, output: 'prompt is required.', assets: [] };
      }
      const size = readString(args, 'size', '1024*1024');
      const count = clamp(readNumber(args, 'count', 1), 1, 4);

      const urls = await media.generateImages({
        prompt,
        model: DEFAULT_IMAGE_MODEL,
        size,
        count,
        ...(context.signal !== undefined ? { signal: context.signal } : {}),
      });

      const assets: StoredAsset[] = [];
      for (const url of urls) {
        assets.push(await media.store('image', url));
      }

      return {
        ok: true,
        output:
          `Generated ${assets.length} image(s) at ${size} with ${DEFAULT_IMAGE_MODEL}. ` +
          'The images are already saved locally and rendered to the user; ' +
          'do not emit markdown image links or raw URLs, just describe them briefly.',
        assets,
      };
    },
  };
}

function createGenerateVideoTool(
  media: MediaService,
  tasks: TaskService,
  conversations: ConversationService,
): AgentTool {
  return {
    spec: GENERATE_VIDEO_SPEC,
    async execute(args, context): Promise<ToolOutcome> {
      const prompt = readString(args, 'prompt', '');
      if (prompt.length === 0) {
        return { ok: false, output: 'prompt is required.', assets: [] };
      }

      const size = readString(args, 'size', DEFAULT_VIDEO_SIZE);
      const duration = clamp(
        readNumber(args, 'duration', DEFAULT_VIDEO_DURATION),
        MIN_VIDEO_DURATION,
        MAX_VIDEO_DURATION,
      );

      // Image-to-video is opt-in: defaulting to "the latest image" would silently
      // turn a plain text-to-video request into an image-to-video one in any
      // conversation that happens to contain a picture.
      const requestedAssetId = readOptionalNumber(args, 'first_frame_asset_id');
      const firstFrame =
        requestedAssetId === null
          ? null
          : conversations.imageAssetIn(context.conversationId, requestedAssetId);

      if (requestedAssetId !== null && firstFrame === null) {
        return {
          ok: false,
          output: `first_frame_asset_id ${requestedAssetId} is not an image in this conversation.`,
          assets: [],
        };
      }
      if (firstFrame !== null && !isPubliclyReachable()) {
        return {
          ok: false,
          output:
            'Image-to-video needs a publicly reachable frame URL, but PUBLIC_BASE_URL points at ' +
            'localhost. Generate the video from the prompt alone, or configure a public base URL.',
          assets: [],
        };
      }

      const { bailian } = loadConfig();
      const model = firstFrame === null ? bailian.videoModelT2v : bailian.videoModelI2v;

      const providerTaskId = await media.submitVideo({
        prompt,
        model,
        size,
        duration,
        ...(firstFrame !== null ? { firstFrameUrl: buildSignedFrameUrl(firstFrame.id) } : {}),
      });

      const taskId = tasks.create({
        conversationId: context.conversationId,
        messageId: context.messageId,
        kind: 'video',
        providerTaskId,
        model,
        prompt,
        params: { size, duration, firstFrameAssetId: firstFrame?.id ?? null },
      });

      const frameNote =
        firstFrame === null ? '' : `，以资产 ${firstFrame.id} 作为首帧`;

      return {
        ok: true,
        output:
          `Video task ${taskId} submitted with ${model} at ${size}, ${duration}s${frameNote}. ` +
          'The render takes a few minutes and the result is pushed to the page automatically. ' +
          'Tell the user the task is under way; do not submit it again.',
        assets: [],
      };
    },
  };
}

function createGetVideoTaskTool(tasks: TaskService): AgentTool {
  return {
    spec: GET_VIDEO_TASK_SPEC,
    async execute(args, context): Promise<ToolOutcome> {
      const taskId = readOptionalNumber(args, 'task_id');
      if (taskId === null) {
        return { ok: false, output: 'task_id is required.', assets: [] };
      }

      const task = tasks.findById(taskId);
      if (task === null || task.conversationId !== context.conversationId) {
        return { ok: false, output: `No video task ${taskId} in this conversation.`, assets: [] };
      }

      const label = TASK_STATUS_TEXT[task.status] ?? task.status;
      const detail = task.error !== null ? `，原因：${task.error}` : '';
      return {
        ok: true,
        output: `Video task ${taskId} is ${label} (${task.status})${detail}.`,
        assets: [],
      };
    },
  };
}

@Injectable()
export class ToolRegistry {
  private readonly tools = new Map<string, AgentTool>();

  constructor(
    media: MediaService,
    tasks: TaskService,
    conversations: ConversationService,
  ) {
    for (const tool of [
      createGenerateImageTool(media),
      createGenerateVideoTool(media, tasks, conversations),
      createGetVideoTaskTool(tasks),
    ]) {
      this.tools.set(tool.spec.name, tool);
    }
  }

  specsFor(mode: Mode): ResponsesFunctionTool[] {
    return TOOL_NAMES_BY_MODE[mode]
      .map((name) => this.tools.get(name)?.spec)
      .filter((spec): spec is ResponsesFunctionTool => spec !== undefined);
  }

  /**
   * Every registered tool, ungated by mode. For the voice session, which runs in
   * none of the four modes (docs/architecture.md §11.3) — `specsFor('auto')` would
   * return the same three today but would silently shrink if 'auto' is ever
   * narrowed.
   */
  allSpecs(): ResponsesFunctionTool[] {
    return [...this.tools.values()].map((tool) => tool.spec);
  }

  get(name: string): AgentTool | undefined {
    return this.tools.get(name);
  }

  /**
   * Runs one tool by name under a hard timeout, forwarding an outer abort.
   *
   * Shared by the text turn loop and the voice session so the two cannot drift
   * on the timeout, the abort forwarding, or the shape of a failure handed back
   * to the model.
   */
  async execute(
    name: string,
    args: Record<string, unknown>,
    context: ToolContext,
  ): Promise<ToolOutcome> {
    const tool = this.tools.get(name);
    if (tool === undefined) {
      return { ok: false, output: `Unknown tool: ${name}`, assets: [] };
    }

    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), TOOL_TIMEOUT_MS);
    const forwardAbort = (): void => controller.abort();
    context.signal?.addEventListener('abort', forwardAbort, { once: true });

    try {
      return await tool.execute(args, {
        conversationId: context.conversationId,
        messageId: context.messageId,
        signal: controller.signal,
      });
    } catch (error) {
      return { ok: false, output: `Tool failed: ${describeError(error)}`, assets: [] };
    } finally {
      clearTimeout(timer);
      context.signal?.removeEventListener('abort', forwardAbort);
    }
  }
}
