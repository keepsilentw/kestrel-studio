import { describe, expect, it } from 'vitest';
import { imageDims } from '@/media/image-dims';

/**
 * The header probe behind `assets.width` / `assets.height`. Same formats and same
 * expectations as `rust/server/src/media/dims.rs`'s tests — the two implementations
 * write one column, so a divergence between them would put two different boxes on the
 * same image depending on which stack stored it.
 */
describe('imageDims', () => {
  it('PNG 从 IHDR 读，含非方形', () => {
    const png = (width: number, height: number): Buffer => {
      const bytes = Buffer.alloc(24);
      Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]).copy(bytes, 0);
      bytes.writeUInt32BE(13, 8);
      bytes.write('IHDR', 12, 'latin1');
      bytes.writeUInt32BE(width, 16);
      bytes.writeUInt32BE(height, 20);
      return bytes;
    };

    expect(imageDims(png(1024, 1024))).toEqual([1024, 1024]);
    expect(imageDims(png(1536, 640))).toEqual([1536, 640]);
  });

  it('JPEG 走到 SOF，高度在前', () => {
    const bytes = Buffer.from([
      0xff, 0xd8,
      0xff, 0xe0, 0x00, 0x04, 0x4a, 0x46, // APP0
      0xff, 0xdb, 0x00, 0x02, // DQT,空载荷
      0xff, 0xc0, 0x00, 0x0b, 0x08, 0x02, 0x80, 0x04, 0x00, // SOF0: 高 640 宽 1024
    ]);

    expect(imageDims(bytes)).toEqual([1024, 640]);
  });

  it('GIF 与 WebP 两种变体', () => {
    const gif = Buffer.concat([Buffer.from('GIF89a', 'latin1'), Buffer.from([0x04, 0x00, 0x02, 0x00])]);
    expect(imageDims(gif)).toEqual([4, 2]);

    const extended = Buffer.concat([
      Buffer.from('RIFF\x00\x00\x00\x00WEBPVP8X\x0a\x00\x00\x00', 'latin1'),
      Buffer.from([0, 0, 0, 0, 0xff, 0x03, 0x00, 0x3f, 0x02, 0x00]),
    ]);
    expect(imageDims(extended)).toEqual([1024, 576]);

    const lossy = Buffer.concat([
      Buffer.from('RIFF\x00\x00\x00\x00WEBPVP8 \x0a\x00\x00\x00', 'latin1'),
      Buffer.from([0x30, 0x01, 0x00, 0x9d, 0x01, 0x2a, 0x00, 0x04, 0x00, 0x02]),
    ]);
    expect(imageDims(lossy)).toEqual([1024, 512]);
  });

  it('认不出的格式返回 null，不编一个数', () => {
    expect(imageDims(Buffer.from(''))).toBeNull();
    expect(imageDims(Buffer.from('not an image at all, not even close'))).toBeNull();
    expect(imageDims(Buffer.from([0x00, 0x00, 0x00, 0x18]))).toBeNull();
    // MP4 是视频：这条探针只被问过图片，就算被问也不该从 moov 里猜。
    expect(imageDims(Buffer.from('\x00\x00\x00\x18ftypmp42\x00\x00\x00\x00mp42isom', 'latin1'))).toBeNull();
  });

  it('截断的头部是 null 而不是抛错', () => {
    const truncated = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d]);

    expect(() => imageDims(truncated)).not.toThrow();
    expect(imageDims(truncated)).toBeNull();
  });
});
