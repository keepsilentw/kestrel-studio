import { describe, expect, it } from 'vitest';
import {
  buildInstructions,
  MODE_LABEL,
  MODES,
  parseMode,
  TOOL_NAMES_BY_MODE,
  type Mode,
} from '@/agent/mode';

const GENERATION_TOOLS = ['generate_image', 'generate_video', 'get_video_task'] as const;

describe('parseMode', () => {
  it('识别四个合法模式', () => {
    expect(parseMode('auto')).toBe('auto');
    expect(parseMode('chat')).toBe('chat');
    expect(parseMode('image')).toBe('image');
    expect(parseMode('video')).toBe('video');
  });

  it('大小写与空白都不敏感', () => {
    expect(parseMode('  VIDEO ')).toBe('video');
    expect(parseMode('Image')).toBe('image');
  });

  // Labelled explicitly: `%s` renders several of these as blanks or as repeated
  // "undefined", which makes a failure message unreadable.
  const invalid: readonly { label: string; value: unknown }[] = [
    { label: '未知模式名', value: 'nope' },
    { label: '空字符串', value: '' },
    { label: '只有空白', value: '   ' },
    { label: 'undefined', value: undefined },
    { label: 'null', value: null },
    { label: '数字', value: 42 },
    { label: '对象', value: {} },
  ];

  it.each(invalid)('$label 回退到 auto', ({ value }) => {
    expect(parseMode(value)).toBe('auto');
  });
});

describe('TOOL_NAMES_BY_MODE', () => {
  it('chat 不给任何工具', () => {
    // The money-spending invariant: a chat turn must not be able to call a
    // generation tool even if the model asks for one.
    expect(TOOL_NAMES_BY_MODE.chat).toEqual([]);
  });

  it('image 只给 generate_image', () => {
    expect(TOOL_NAMES_BY_MODE.image).toEqual(['generate_image']);
    expect(TOOL_NAMES_BY_MODE.image).not.toContain('generate_video');
  });

  it('video 只给两个视频工具，不给 generate_image', () => {
    expect(TOOL_NAMES_BY_MODE.video).toEqual(['generate_video', 'get_video_task']);
    expect(TOOL_NAMES_BY_MODE.video).not.toContain('generate_image');
  });

  it('auto 给出全部工具，交给模型自选', () => {
    expect([...TOOL_NAMES_BY_MODE.auto].sort()).toEqual([...GENERATION_TOOLS].sort());
  });

  it('每个模式都有工具条目（哪怕是空数组）', () => {
    for (const mode of MODES) {
      expect(TOOL_NAMES_BY_MODE[mode]).toBeDefined();
    }
  });
});

describe('buildInstructions', () => {
  it.each(MODES)('%s 模式都能生成非空提示词', (mode) => {
    expect(buildInstructions(mode).length).toBeGreaterThan(0);
  });

  it('每个模式都带上基础规则', () => {
    for (const mode of MODES) {
      const instructions = buildInstructions(mode);
      expect(instructions).toContain('用中文回复用户');
      // Assets render themselves; markdown image syntax would duplicate them.
      expect(instructions).toContain('不要输出 markdown 图片语法');
    }
  });

  it('chat 模式明确说明没有工具，并要求用户切换模式', () => {
    const instructions = buildInstructions('chat');
    expect(instructions).toContain('不提供任何生成工具');
    expect(instructions).toContain('切换到对应的模式');
  });

  it('video 模式点名 get_video_task 并禁止提交后轮询', () => {
    // The tool is still advertised in video mode, so the instructions are the
    // only thing stopping the model from polling a job the server already pushes.
    const instructions = buildInstructions('video');
    expect(instructions).toContain('get_video_task');
    expect(instructions).toContain('不要反复调用');
  });

  it('image 模式点名 generate_image', () => {
    expect(buildInstructions('image')).toContain('generate_image');
  });

  it('视频与图片的提示词不同 —— 模式确实切换了提示词片段', () => {
    expect(buildInstructions('image')).not.toBe(buildInstructions('video'));
  });
});

describe('MODE_LABEL', () => {
  it('每个模式都有中文标签', () => {
    for (const mode of MODES) {
      expect(MODE_LABEL[mode].length).toBeGreaterThan(0);
    }
  });

  it('覆盖全部模式，不多不少', () => {
    expect(Object.keys(MODE_LABEL).sort()).toEqual([...MODES].sort());
  });
});

describe('MODES', () => {
  it('与 Mode 联合类型一致', () => {
    const asRecord: Record<Mode, true> = { auto: true, chat: true, image: true, video: true };
    expect([...MODES].sort()).toEqual(Object.keys(asRecord).sort());
  });
});
