/**
 * The pixel dimensions of a generated image, read from its own bytes.
 *
 * Same file, same rules as `rust/server/src/media/dims.rs` — read that one first; it
 * carries the reasoning (a lazy `<img>` with no dimensions reserves no box, and the
 * request's `size` is not a usable substitute because the provider snaps it).
 *
 * Why this is hand-written rather than a package: an image library is not a dependency
 * either stack has, and adding one here means a `package.json` plus `pnpm-lock.yaml`
 * change, which this repository does not do casually (CLAUDE.md: lock files are not
 * touched). What is needed is four magic numbers, not a codec.
 */

/** `[width, height]`, or null for anything whose header we cannot read. */
export function imageDims(bytes: Buffer): [number, number] | null {
  if (bytes.length < 10) {
    return null;
  }
  // PNG: signature, then IHDR as the first chunk — 4 length, 4 type, 4 width, 4 height.
  if (bytes.subarray(0, 8).equals(Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]))) {
    if (bytes.subarray(12, 16).toString('latin1') !== 'IHDR' || bytes.length < 24) {
      return null;
    }
    return [bytes.readUInt32BE(16), bytes.readUInt32BE(20)];
  }
  if (bytes.subarray(0, 4).toString('latin1') === 'GIF8') {
    return [bytes.readUInt16LE(6), bytes.readUInt16LE(8)];
  }
  if (bytes[0] === 0xff && bytes[1] === 0xd8) {
    return jpegDims(bytes);
  }
  if (
    bytes.subarray(0, 4).toString('latin1') === 'RIFF' &&
    bytes.subarray(8, 12).toString('latin1') === 'WEBP'
  ) {
    return webpDims(bytes);
  }
  return null;
}

/** Walks JPEG marker segments to the first start-of-frame one. */
function jpegDims(bytes: Buffer): [number, number] | null {
  let at = 2;
  while (at + 4 <= bytes.length) {
    if (bytes[at] !== 0xff) {
      return null;
    }
    const marker = bytes[at + 1];
    // Standalone markers carry no length field.
    if (marker === 0x01 || (marker >= 0xd0 && marker <= 0xd9)) {
      at += 2;
      continue;
    }
    const length = bytes.readUInt16BE(at + 2);
    if (length < 2) {
      return null;
    }
    const isFrame = marker >= 0xc0 && marker <= 0xcf && marker !== 0xc4 && marker !== 0xc8 && marker !== 0xcc;
    if (isFrame) {
      // Height first inside a JPEG frame header — the opposite of PNG's order, which is
      // how this function earns its comment.
      const height = bytes.readUInt16BE(at + 5);
      const width = bytes.readUInt16BE(at + 7);
      return width > 0 && height > 0 ? [width, height] : null;
    }
    at += 2 + length;
  }
  return null;
}

function webpDims(bytes: Buffer): [number, number] | null {
  let at = 12;
  while (at + 8 <= bytes.length) {
    const tag = bytes.subarray(at, at + 4).toString('latin1');
    const size = bytes.readUInt32LE(at + 4);
    const data = at + 8;
    const end = data + size;
    if (end > bytes.length) {
      return null;
    }
    if (tag === 'VP8X') {
      return [bytes.readUIntLE(data + 4, 3) + 1, bytes.readUIntLE(data + 7, 3) + 1];
    }
    if (tag === 'VP8L') {
      const bits = bytes.readUInt32BE(data + 1);
      return [(bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1];
    }
    if (tag === 'VP8 ') {
      const window = bytes.subarray(data, Math.min(end, data + 32));
      const found = window.indexOf(Buffer.from([0x9d, 0x01, 0x2a]));
      if (found < 0) {
        return null;
      }
      const at = data + found + 3;
      const width = bytes.readUInt16LE(at) & 0x3fff;
      const height = bytes.readUInt16LE(at + 2) & 0x3fff;
      return width > 0 && height > 0 ? [width, height] : null;
    }
    at = end + (size % 2);
  }
  return null;
}
