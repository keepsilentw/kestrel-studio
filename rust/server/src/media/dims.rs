//! The pixel dimensions of a generated image, read from its own bytes.
//!
//! Why this exists at all: a replayed conversation renders its images with
//! `loading="lazy"`, and a lazy `<img>` without `width`/`height` reserves no box, so
//! every image reflows the page under it. The fix is the two attributes, and the two
//! attributes need the real numbers.
//!
//! Why it reads the header instead of trusting the request: measured against the
//! production library on 2026-11-09, of 19 stored images one had been asked for as
//! `1024*448` and came back `1536x640` — the provider snaps to the sizes it supports.
//! A wrong aspect ratio in `width`/`height` distorts the reserved box, which is worse
//! than the reflow. So the header is the only acceptable source, and anything this
//! cannot parse returns `None` and leaves the attributes off.
//!
//! No dependency: neither stack has an image decoder, and adding one to the Rust
//! server means shipping it to every user (this binary is 9.2 MB static, and
//! `docs/verification.md` §11.5 is the precedent for refusing a markdown library on
//! exactly that ground), while adding one to the Nest stack means a lockfile change,
//! which this repository does not do casually. What is needed here is four magic
//! numbers, not a codec — and the four formats below are exactly the extensions
//! `media::service::ALLOWED_EXTENSIONS` will accept for an image.

/// `(width, height)` if these bytes are an image format we can read the size from.
pub fn image_dims(bytes: &[u8]) -> Option<(u32, u32)> {
    // No blanket length guard: the shortest header here is a GIF's 10 bytes, and
    // every read below goes through `get`, so a truncated file answers `None` on its
    // own rather than by an early return that would also reject valid short headers.
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        // IHDR is the first chunk: 4 length, 4 type, then width and height, big-endian.
        if bytes.get(12..16) != Some(&b"IHDR"[..]) {
            return None;
        }
        return Some((be32(bytes, 16)?, be32(bytes, 20)?));
    }
    if bytes.starts_with(b"GIF8") {
        return Some((le16(bytes, 6)?, le16(bytes, 8)?));
    }
    if bytes.starts_with(b"\xff\xd8") {
        return jpeg_dims(bytes);
    }
    if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        return webp_dims(bytes);
    }
    None
}

fn be32(bytes: &[u8], at: usize) -> Option<u32> {
    bytes
        .get(at..at + 4)
        .map(|four| u32::from_be_bytes([four[0], four[1], four[2], four[3]]))
}

fn le16(bytes: &[u8], at: usize) -> Option<u32> {
    bytes
        .get(at..at + 2)
        .map(|two| u16::from_le_bytes([two[0], two[1]]) as u32)
}

fn be16(bytes: &[u8], at: usize) -> Option<u32> {
    bytes
        .get(at..at + 2)
        .map(|two| u16::from_be_bytes([two[0], two[1]]) as u32)
}

/// Walks the JPEG marker segments until a start-of-frame one carries the size.
///
/// SOF1-SOF3 and SOF5-SOF7 / SOF9-SOF11 / SOF13-SOF15 are the arithmetic and
/// differential variants; SOF4 (DHT) and SOF8 (JPG), SOF12 (DAC) and SOF0 (baseline)
/// are excluded where they do not carry a frame. Inside the segment: 2 length, 1
/// precision, then height and width, big-endian — height first, which is the easy
/// one to get backwards.
fn jpeg_dims(bytes: &[u8]) -> Option<(u32, u32)> {
    let mut at = 2;
    while at + 4 <= bytes.len() {
        if bytes[at] != 0xff {
            return None;
        }
        let marker = bytes[at + 1];
        // Standalone markers with no payload: RST0-RST7, SOI, EOI, and the fill bytes.
        if marker == 0x01 || (0xd0..=0xd9).contains(&marker) {
            at += 2;
            continue;
        }
        let length = be16(bytes, at + 2)? as usize;
        if length < 2 {
            return None;
        }
        let is_frame = (0xc0..=0xcf).contains(&marker) && !matches!(marker, 0xc4 | 0xc8 | 0xcc);
        if is_frame {
            let height = be16(bytes, at + 5)?;
            let width = be16(bytes, at + 7)?;
            if width > 0 && height > 0 {
                return Some((width, height));
            }
            return None;
        }
        at += 2 + length;
    }
    None
}

/// RIFF container: four-byte tag, four-byte little-endian size, then the payload.
fn webp_dims(bytes: &[u8]) -> Option<(u32, u32)> {
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let tag = &bytes[at..at + 4];
        // Chunk headers are little-endian, unlike everything else in this file.
        let size = u32::from_le_bytes([bytes[at + 4], bytes[at + 5], bytes[at + 6], bytes[at + 7]]) as usize;
        let data = at + 8;
        let end = data + size;
        if end > bytes.len() {
            return None;
        }
        match tag {
            b"VP8X" => {
                // 10 flags bytes are not needed; width and height follow as 3-byte
                // little-endian values, each stored minus one.
                let width = le24(bytes, data + 4)? + 1;
                let height = le24(bytes, data + 7)? + 1;
                return Some((width, height));
            }
            b"VP8L" => {
                // Lossless: 1 signature byte, then 4 bytes holding width-1 and
                // height-1 in 14 bits each.
                let bits = be32(bytes, data + 1)?;
                let width = (bits & 0x3fff) + 1;
                let height = ((bits >> 14) & 0x3fff) + 1;
                return Some((width, height));
            }
            b"VP8 " => {
                // Lossy: find the three-byte sync code inside the frame header.
                let window = &bytes[data..end.min(data + 32)];
                if let Some(found) = window.windows(3).position(|w| w == [0x9d, 0x01, 0x2a]) {
                    let at = data + found + 3;
                    let width = le16(bytes, at)? & 0x3fff;
                    let height = le16(bytes, at + 2)? & 0x3fff;
                    if width > 0 && height > 0 {
                        return Some((width, height));
                    }
                }
                return None;
            }
            _ => {}
        }
        // Chunks are word-aligned: an odd size pads with one byte.
        at = end + (size % 2);
    }
    None
}

fn le24(bytes: &[u8], at: usize) -> Option<u32> {
    let three = bytes.get(at..at + 3)?;
    Some(u32::from(three[0]) | (u32::from(three[1]) << 8) | (u32::from(three[2]) << 16))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real PNG header, byte for byte: signature, IHDR length and type, then the
    /// two big-endian dimensions. Everything after the header is irrelevant here.
    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        bytes.extend_from_slice(&13u32.to_be_bytes());
        bytes.extend_from_slice(b"IHDR");
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&[0u8; 9]);
        bytes
    }

    #[test]
    fn png_is_read_from_ihdr() {
        assert_eq!(image_dims(&png(1024, 1024)), Some((1024, 1024)));
        assert_eq!(image_dims(&png(1536, 640)), Some((1536, 640)));
    }

    #[test]
    fn jpeg_is_found_by_walking_to_the_start_of_frame() {
        // APP0 (JFIF) then a baseline SOF0. Height comes first inside the segment —
        // the one place in this file where the order is not width-then-height.
        let mut bytes = vec![0xff, 0xd8];
        bytes.extend_from_slice(&[0xff, 0xe0, 0x00, 0x04, b'J', b'F']);
        bytes.extend_from_slice(&[0xff, 0xdb, 0x00, 0x02]); // DQT, empty payload
        bytes.extend_from_slice(&[0xff, 0xc0, 0x00, 0x0b, 8, 0x02, 0x80, 0x04, 0x00]);
        assert_eq!(image_dims(&bytes), Some((1024, 640)));
    }

    #[test]
    fn jpeg_without_a_frame_segment_is_not_guessed() {
        let bytes = vec![0xff, 0xd8, 0xff, 0xd9];
        assert_eq!(image_dims(&bytes), None);
    }

    #[test]
    fn gif_and_webp_variants_are_read() {
        let mut gif = b"GIF89a".to_vec();
        gif.extend_from_slice(&[0x04, 0x00, 0x02, 0x00]);
        assert_eq!(image_dims(&gif), Some((4, 2)));

        // Chunk header is a 4-byte tag plus a **4-byte** little-endian size. Getting
        // that width wrong is the classic way to write a fixture that silently tests
        // the "truncated file" path instead of the format.
        let mut extended = b"RIFF\x00\x00\x00\x00WEBPVP8X\x0a\x00\x00\x00".to_vec();
        extended.extend_from_slice(&[0; 4]); // flags, 10-byte payload = 4 + 3 + 3
        extended.extend_from_slice(&[0xff, 0x03, 0x00]); // width-1 = 1023
        extended.extend_from_slice(&[0x3f, 0x02, 0x00]); // height-1 = 575
        assert_eq!(image_dims(&extended), Some((1024, 576)));

        // Lossy VP8 payload: 3-byte frame tag, the 0x9d 0x01 0x2a sync, then the two
        // 14-bit little-endian dimensions. 10 bytes, which is what the size says.
        let mut lossy = b"RIFF\x00\x00\x00\x00WEBPVP8 \x0a\x00\x00\x00".to_vec();
        lossy.extend_from_slice(&[0x30, 0x01, 0x00]);
        lossy.extend_from_slice(&[0x9d, 0x01, 0x2a]);
        lossy.extend_from_slice(&[0x00, 0x04, 0x00, 0x02]); // width 1024, height 512
        assert_eq!(image_dims(&lossy), Some((1024, 512)));
    }

    #[test]
    fn anything_else_is_none_not_a_guess() {
        // The whole point of returning Option: an unrecognised format leaves the
        // attributes off rather than reserving a box with made-up numbers.
        assert_eq!(image_dims(b""), None);
        assert_eq!(image_dims(b"not an image at all, not even close"), None);
        assert_eq!(image_dims(&[0u8; 4]), None);
        // An MP4 is a video: this file is only asked about images, and even if it
        // were, guessing from a `moov` box is not what this is for.
        assert_eq!(image_dims(b"\x00\x00\x00\x18ftypmp42\x00\x00\x00\x00mp42isom"), None);
    }

    #[test]
    fn a_truncated_header_is_none_instead_of_a_panic() {
        let mut bytes = png(1024, 1024);
        bytes.truncate(18);
        assert_eq!(image_dims(&bytes), None);
    }
}
