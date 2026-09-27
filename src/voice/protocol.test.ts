import { describe, expect, it } from 'vitest';
import { parseClientFrame } from '@/voice/protocol';

/**
 * The peer is a browser, so a malformed frame must come back as null rather
 * than a throw: one bad frame should not take the call down.
 */
describe('parseClientFrame', () => {
  it('识别三个无载荷的控制帧', () => {
    expect(parseClientFrame('{"type":"start"}')).toEqual({ type: 'start' });
    expect(parseClientFrame('{"type":"commit"}')).toEqual({ type: 'commit' });
    expect(parseClientFrame('{"type":"stop"}')).toEqual({ type: 'stop' });
  });

  it('带载荷的音频帧原样保留 base64', () => {
    expect(parseClientFrame('{"type":"audio","pcm":"AAECAw=="}')).toEqual({
      type: 'audio',
      pcm: 'AAECAw==',
    });
  });

  it('音频帧缺 pcm 或 pcm 为空时判为无效', () => {
    expect(parseClientFrame('{"type":"audio"}')).toBeNull();
    expect(parseClientFrame('{"type":"audio","pcm":""}')).toBeNull();
    expect(parseClientFrame('{"type":"audio","pcm":42}')).toBeNull();
  });

  it('不认识 type 时返回 null', () => {
    expect(parseClientFrame('{"type":"cancel"}')).toBeNull();
    expect(parseClientFrame('{"pcm":"AA=="}')).toBeNull();
  });

  it('非法 JSON、非对象、数组一律返回 null', () => {
    expect(parseClientFrame('not json')).toBeNull();
    expect(parseClientFrame('')).toBeNull();
    expect(parseClientFrame('null')).toBeNull();
    expect(parseClientFrame('42')).toBeNull();
    expect(parseClientFrame('"start"')).toBeNull();
    expect(parseClientFrame('[{"type":"start"}]')).toBeNull();
  });

  it('多余字段被忽略，不影响判定', () => {
    expect(parseClientFrame('{"type":"commit","extra":1}')).toEqual({ type: 'commit' });
  });
});
