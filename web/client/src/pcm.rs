//! PCM and base64, with no browser in sight.
//!
//! The voice protocol is base64 of raw PCM16 **little-endian**, mono, 24 kHz, in
//! both directions (`ClientFrame::Audio` up, `ServerFrame::Audio` down). Two
//! properties are worth pinning here instead of trusting the browser:
//!
//! * endianness. `Int16Array` over an `ArrayBuffer` is platform-endian, and every
//!   engine this runs on is little-endian — but an explicit writer does not need
//!   that assumption, and the upstream one does.
//! * an odd byte count. `playChunk` did `Math.floor(byteLength / 2)` and dropped
//!   the last byte silently; the same choice is made visible here.
//!
//! This is the part of the audio path that can be tested at all on a machine that
//! may not open a browser (CLAUDE.md: no services, no Docker locally).

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding, as the voice protocol's frames carry it.
pub fn to_base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(triple >> 18) as usize & 0x3f] as char);
        out.push(ALPHABET[(triple >> 12) as usize & 0x3f] as char);
        out.push(if chunk.len() > 1 { ALPHABET[(triple >> 6) as usize & 0x3f] as char } else { '=' });
        out.push(if chunk.len() > 2 { ALPHABET[triple as usize & 0x3f] as char } else { '=' });
    }
    out
}

/// `None` on anything that is not valid base64, rather than a silent partial
/// decode: a corrupt frame should surface as a dropped frame plus a message, not
/// as a click in the playback.
pub fn from_base64(text: &str) -> Option<Vec<u8>> {
    let body = text.trim_end_matches('=');
    let mut out = Vec::with_capacity(body.len() * 3 / 4);
    let mut buffer: u32 = 0;
    let mut bits = 0u32;

    for character in body.chars() {
        let value = match character {
            'A'..='Z' => character as u32 - 'A' as u32,
            'a'..='z' => character as u32 - 'a' as u32 + 26,
            '0'..='9' => character as u32 - '0' as u32 + 52,
            '+' => 62,
            '/' => 63,
            // Whitespace inside a frame is not in the protocol; treat it as
            // corruption instead of being forgiving about it.
            _ => return None,
        };
        buffer = (buffer << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8 & 0xff);
        }
    }
    Some(out)
}

/// PCM16 little-endian bytes to the `-1.0..1.0` floats `AudioBuffer` wants.
///
/// Scaled by `1/0x8000` for both signs, exactly as `playChunk` did: asymmetric
/// scaling would shift the waveform's centre.
pub fn pcm16_le_to_f32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks(2)
        .filter_map(|pair| {
            if pair.len() < 2 {
                return None;
            }
            let sample = i16::from_le_bytes([pair[0], pair[1]]);
            Some(f32::from(sample) / 32768.0)
        })
        .collect()
}

/// Float samples to PCM16 little-endian bytes: the inverse scaling the capture
/// worklet performs in JS (`sample < 0 ? *0x8000 : *0x7fff`), kept here so the
/// two directions can be tested against each other.
pub fn f32_to_pcm16_le(samples: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 2);
    for sample in samples {
        let clamped = sample.clamp(-1.0, 1.0);
        let scaled = if clamped < 0.0 { clamped * 32768.0 } else { clamped * 32767.0 };
        let value = scaled.round().clamp(-32768.0, 32767.0) as i16;
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_reference_vectors() {
        // RFC 4648 sample strings.
        assert_eq!(to_base64(b""), "");
        assert_eq!(to_base64(b"f"), "Zg==");
        assert_eq!(to_base64(b"fo"), "Zm8=");
        assert_eq!(to_base64(b"foo"), "Zm9v");
        assert_eq!(to_base64(b"foob"), "Zm9vYg==");
        assert_eq!(to_base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(to_base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn base64_decodes_what_it_encoded() {
        let bytes: Vec<u8> = (0..=255u8).collect();
        let encoded = to_base64(&bytes);
        assert_eq!(from_base64(&encoded).as_deref(), Some(bytes.as_slice()));
    }

    #[test]
    fn base64_rejects_characters_that_are_not_in_the_alphabet() {
        assert_eq!(from_base64("Zm9v\nYmE"), None);
        assert_eq!(from_base64("Zm9*"), None);
        assert_eq!(from_base64("中文"), None);
    }

    #[test]
    fn pcm_is_little_endian_and_signed() {
        // -1, +1, the two extremes.
        let samples = vec![-1.0f32, 1.0, -0.5, 0.5];
        let bytes = f32_to_pcm16_le(&samples);
        // Little-endian, and the two halves are asymmetric like the worklet:
        // -1.0 lands on -32768 (0x8000 → 00 80), +1.0 tops out at +32767
        // (0x7FFF → FF 7F), -0.5 is -16384 (two's complement 0xC000 → 00 C0),
        // +0.5 is +16384 once `round` goes away from zero (0x4000 → 00 40).
        assert_eq!(bytes, vec![0x00, 0x80, 0xff, 0x7f, 0x00, 0xc0, 0x00, 0x40]);
        let back = pcm16_le_to_f32(&bytes);
        assert_eq!(back.len(), samples.len());
        assert!((back[0] + 1.0).abs() < 1e-4, "the floor of the range stays -1.0");
        assert!((back[1] - 1.0).abs() < 1e-4);
    }

    #[test]
    fn an_odd_trailing_byte_is_dropped_not_misread() {
        // `playChunk` did `floor(byteLength / 2)`; the same shape has to hold or
        // the last sample of a stream would be a byte pair made of garbage.
        let decoded = from_base64(&to_base64(&[0x01])).expect("decodes");
        assert_eq!(pcm16_le_to_f32(&decoded), Vec::<f32>::new());
    }

    #[test]
    fn out_of_range_samples_are_clamped() {
        let bytes = f32_to_pcm16_le(&[3.0, -3.0]);
        assert_eq!(bytes, vec![0xff, 0x7f, 0x00, 0x80]);
    }

    #[test]
    fn the_frame_the_protocol_uses_round_trips() {
        // 1024 samples, which is the worklet's message size at 24 kHz.
        let samples = vec![0.25f32; 1024];
        let frame = to_base64(&f32_to_pcm16_le(&samples));
        assert_eq!(frame.len(), 1024 * 2 * 4 / 3 + 2, "2048 bytes padded to base64");
        let bytes = from_base64(&frame).expect("valid base64");
        assert_eq!(bytes.len(), 2048);
        assert_eq!(pcm16_le_to_f32(&bytes).len(), 1024);
    }
}
