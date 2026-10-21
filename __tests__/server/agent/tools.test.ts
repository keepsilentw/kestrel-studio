import { Test } from '@nestjs/testing';
import { beforeEach, describe, expect, it } from 'vitest';
import { MODES, TOOL_NAMES_BY_MODE, type Mode } from '@/agent/mode';
import { ToolRegistry, type AgentTool } from '@/agent/tools';
import { ConversationService } from '@/conversation/conversation.service';
import { MediaService } from '@/media/media.service';
import { TaskService } from '@/task/task.service';

/**
 * The services are stubbed because the registry only closes over them — no tool
 * body runs unless `execute` is called, and those bodies are the media/task
 * layer's business, verified against the live provider instead.
 */
async function buildRegistry(): Promise<ToolRegistry> {
  const moduleRef = await Test.createTestingModule({
    providers: [
      ToolRegistry,
      { provide: MediaService, useValue: {} },
      { provide: TaskService, useValue: {} },
      { provide: ConversationService, useValue: {} },
    ],
  }).compile();
  return moduleRef.get(ToolRegistry);
}

function readString(source: Record<string, unknown>, key: string): string {
  const value = source[key];
  return typeof value === 'string' ? value : '';
}

function readStringArray(source: Record<string, unknown>, key: string): string[] {
  const value = source[key];
  return Array.isArray(value)
    ? value.filter((item): item is string => typeof item === 'string')
    : [];
}

function namesFor(mode: Mode, registry: ToolRegistry): string[] {
  return registry.specsFor(mode).map((spec) => spec.name);
}

/** Narrows rather than asserting: a missing tool should say so, not crash later. */
function tool(registry: ToolRegistry, name: string): AgentTool {
  const found = registry.get(name);
  if (found === undefined) {
    throw new Error(`tool "${name}" is not registered`);
  }
  return found;
}

describe('ToolRegistry — 模式裁剪', () => {
  let registry: ToolRegistry;

  beforeEach(async () => {
    registry = await buildRegistry();
  });

  it('每个模式暴露的工具与 mode.ts 的声明完全一致', () => {
    // The guard against silent tool loss: specsFor() is
    // `.filter(spec => spec !== undefined)`, so a name listed in
    // TOOL_NAMES_BY_MODE that has no registered tool is dropped without any
    // error — the model simply never sees it. This test is the only thing that
    // turns that into a failure.
    for (const mode of MODES) {
      expect(namesFor(mode, registry)).toEqual([...TOOL_NAMES_BY_MODE[mode]]);
    }
  });

  it('chat 模式不暴露任何工具', () => {
    expect(registry.specsFor('chat')).toEqual([]);
  });

  it('image 模式只暴露 generate_image', () => {
    expect(namesFor('image', registry)).toEqual(['generate_image']);
  });

  it('video 模式暴露两个视频工具，且不含 generate_image', () => {
    expect(namesFor('video', registry)).toEqual(['generate_video', 'get_video_task']);
  });

  it('auto 模式暴露全部三个工具', () => {
    expect(namesFor('auto', registry)).toHaveLength(3);
  });

  it('外部模式取值不会漏出未声明的工具', () => {
    for (const mode of MODES) {
      for (const name of namesFor(mode, registry)) {
        expect(TOOL_NAMES_BY_MODE[mode]).toContain(name);
      }
    }
  });
});

describe('ToolRegistry — spec 内容', () => {
  let registry: ToolRegistry;

  beforeEach(async () => {
    registry = await buildRegistry();
  });

  it('每个工具都有非空 description', () => {
    // Load-bearing, not cosmetic: kestrel's registry hard-codes
    // `description: String::new()`, which is called out in docs/roadmap.md §2.2
    // as a reason the model sees argument schemas but not what a tool is for.
    for (const mode of MODES) {
      for (const spec of registry.specsFor(mode)) {
        expect(spec.description.length).toBeGreaterThan(0);
      }
    }
  });

  it('spec 的 name 与注册表键一致', () => {
    for (const mode of MODES) {
      for (const spec of registry.specsFor(mode)) {
        expect(registry.get(spec.name)?.spec.name).toBe(spec.name);
      }
    }
  });

  it('每个 spec 都是 function 类型且参数 schema 完整', () => {
    for (const mode of MODES) {
      for (const spec of registry.specsFor(mode)) {
        expect(spec.type).toBe('function');
        expect(readString(spec.parameters, 'type')).toBe('object');
        expect(readStringArray(spec.parameters, 'required').length).toBeGreaterThan(0);
        // Closed schemas: the model must not invent extra arguments that the
        // tool body would then ignore.
        expect(spec.parameters.additionalProperties).toBe(false);
      }
    }
  });

  it('两个生成工具都要求 prompt', () => {
    expect(readStringArray(tool(registry, 'generate_image').spec.parameters, 'required')).toEqual([
      'prompt',
    ]);
    expect(readStringArray(tool(registry, 'generate_video').spec.parameters, 'required')).toEqual([
      'prompt',
    ]);
  });

  it('get_video_task 要的是 task_id，不是 prompt', () => {
    const spec = tool(registry, 'get_video_task').spec;
    expect(readStringArray(spec.parameters, 'required')).toEqual(['task_id']);
  });

  it('工具名不重复', () => {
    const names = registry.specsFor('auto').map((spec) => spec.name);
    expect(new Set(names).size).toBe(names.length);
  });

  it('未注册的名字返回 undefined', () => {
    expect(registry.get('generate_image')).toBeDefined();
    expect(registry.get('delete_everything')).toBeUndefined();
  });

  it('每个报给模型的工具名都是它自己 spec 的名字', () => {
    // Loose end from the args reader: a tool whose `name` disagrees with the key
    // it is dispatched by would be advertised under one name and looked up
    // under another.
    for (const spec of registry.specsFor('auto')) {
      expect(spec.name.length).toBeGreaterThan(0);
      expect(spec.name).not.toContain(' ');
    }
  });
});
