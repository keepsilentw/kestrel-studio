/**
 * A turn's capability mode. The selector is per turn, not per conversation:
 * one conversation can chat, then draw, then animate, and each turn is stored
 * with the mode it ran in so history renders correctly.
 *
 * `auto` exposes every tool and lets the model route itself; the explicit modes
 * narrow the tool set so a turn cannot silently spend money on a generation the
 * user did not ask for.
 */
export type Mode = 'auto' | 'chat' | 'image' | 'video';

export const DEFAULT_MODE: Mode = 'auto';

export const MODES: readonly Mode[] = ['auto', 'chat', 'image', 'video'];

export const MODE_LABEL: Record<Mode, string> = {
  auto: '智能',
  chat: '对话',
  image: '图片',
  video: '视频',
};

/** Tool names advertised to the model per mode. `chat` deliberately has none. */
export const TOOL_NAMES_BY_MODE: Record<Mode, readonly string[]> = {
  auto: ['generate_image', 'generate_video', 'get_video_task'],
  chat: [],
  image: ['generate_image'],
  video: ['generate_video', 'get_video_task'],
};

export function parseMode(value: unknown): Mode {
  if (typeof value !== 'string') {
    return DEFAULT_MODE;
  }
  const normalized = value.trim().toLowerCase();
  for (const mode of MODES) {
    if (mode === normalized) {
      return mode;
    }
  }
  return DEFAULT_MODE;
}

const BASE_INSTRUCTIONS = [
  '你是一个生成式媒体助手，运行在一个网页应用里。',
  '',
  '规则：',
  '- 用中文回复用户。',
  '- 生成前先用一两句话说明你的构思（这一步会被实时展示给用户）。',
  '- 生成的图片或视频会自动渲染到页面上，不要输出 markdown 图片语法，也不要贴 URL。',
  '- 生成后用一两句话简要说明你产出了什么。',
];

const MODE_INSTRUCTIONS: Record<Mode, string[]> = {
  auto: [
    '当前是智能模式：根据用户意图自行判断是闲聊、生成图片还是生成视频。',
    '如果用户只是在闲聊或提问，不涉及生成，就直接回答，不要调用工具。',
  ],
  chat: [
    '当前是纯对话模式：本次对话不提供任何生成工具。',
    '如果用户要求生成图片或视频，告诉他切换到对应的模式后再试，不要假装已经生成。',
  ],
  image: [
    '当前是图片模式：用户想要图片时，调用 generate_image。',
  ],
  video: [
    '当前是视频模式：用户想要视频时，调用 generate_video。',
    '文生视频直接传 prompt；图生视频需要额外传 first_frame_asset_id，',
    '该 id 是之前调用 generate_image 时工具结果里给出的 asset id。',
    '视频生成是异步任务，提交后会立刻返回，完成后服务端会自动把结果推送到页面。',
    '因此提交后不要反复调用 get_video_task 轮询，直接告诉用户任务已提交即可。',
  ],
};

export function buildInstructions(mode: Mode): string {
  return [...BASE_INSTRUCTIONS, '', ...MODE_INSTRUCTIONS[mode]].join('\n');
}
