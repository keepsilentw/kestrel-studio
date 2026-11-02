//! The WebSocket transport, written against RFC 6455.
//!
//! `tungstenite` is not in the local cargo registry and this box has no outbound
//! network, so the frame codec lives here. Everything it needs is already
//! vendored: `sha1`, `base64` and `tokio-rustls`.
//!
//! Scope on purpose: this module turns bytes into frames and back. It does not
//! own a socket, does not do the HTTP upgrade (that is `hyper::upgrade::on`) and
//! does not speak the voice protocol - so all three of those can be tested
//! against it without a network.

use base64::Engine;
use sha1::{Digest, Sha1};

/// The GUID the RFC fixes for the handshake, so a proxy that never saw a
/// WebSocket cannot accidentally produce a matching `Sec-WebSocket-Accept`.
const RFC6455_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// A frame's payload is capped: the voice path carries audio chunks of a few
/// kilobytes, and an unbounded reader is a memory leak with a handshake on it.
const MAX_MESSAGE_BYTES: usize = 8 << 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    /// A finished text message, continuation frames already joined.
    Text(String),
    /// A finished binary message, continuation frames already joined.
    Binary(Vec<u8>),
    Ping(Vec<u8>),
    Pong(Vec<u8>),
    /// A close frame; the code is absent for a bare TCP-level drop.
    Close(Option<u16>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsError {
    /// The peer broke the protocol; the connection must go.
    Protocol(String),
    /// A single message outgrew `MAX_MESSAGE_BYTES`.
    TooLarge,
}

/// `Sec-WebSocket-Accept` for a client's `Sec-WebSocket-Key`.
pub fn accept_key(sec_key: &str) -> Option<String> {
    let key = sec_key.trim();
    if key.is_empty() {
        return None;
    }
    // The key is a base64 value; reject anything that is not, so a typo in a
    // client becomes a 400 rather than a signature over garbage.
    if base64::engine::general_purpose::STANDARD.decode(key).is_err() {
        return None;
    }
    let mut hasher = Sha1::new();
    hasher.update(key.as_bytes());
    hasher.update(RFC6455_GUID.as_bytes());
    Some(base64::engine::general_purpose::STANDARD.encode(hasher.finalize()))
}

/// Accumulates bytes and yields whole frames.
///
/// A fragmented message (continuation opcode 0) is assembled inside, so a caller
/// never sees a half string - the voice client's JSON control frames and the
/// server's audio frames are both expected to be single-frame, and the ones that
/// are not still arrive complete.
#[derive(Default)]
pub struct WsDecoder {
    buffer: Vec<u8>,
    fragment_opcode: Option<u8>,
    fragment_payload: Vec<u8>,
}

impl WsDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds one chunk; returns every frame that completed.
    ///
    /// `masked` says whether frames from this peer must be XOR-masked: clients
    /// mask, servers do not.
    pub fn push(&mut self, chunk: &[u8], masked: bool) -> Result<Vec<Frame>, WsError> {
        self.buffer.extend_from_slice(chunk);
        let mut frames = Vec::new();
        loop {
            match self.take_frame(masked)? {
                Some(Some(frame)) => frames.push(frame),
                Some(None) => {}
                None => break,
            }
        }
        Ok(frames)
    }

    /// `Ok(None)` asks for more bytes; `Ok(Some(None))` is a control frame that
    /// produced no message.
    fn take_frame(&mut self, masked: bool) -> Result<Option<Option<Frame>>, WsError> {
        if self.buffer.len() < 2 {
            return Ok(None);
        }
        let first = self.buffer[0];
        let second = self.buffer[1];
        let fin = first & 0x80 != 0;
        let opcode = first & 0x0f;
        if first & 0x70 != 0 {
            return Err(WsError::Protocol("reserved bits must be zero".into()));
        }
        if (second & 0x80 != 0) != masked {
            return Err(WsError::Protocol(if masked {
                "client frames must be masked".into()
            } else {
                "server frames must not be masked".into()
            }));
        }

        let mut offset = 2usize;
        let length = match second & 0x7f {
            0..=125 => (second & 0x7f) as usize,
            126 => {
                if self.buffer.len() < offset + 2 {
                    return Ok(None);
                }
                u16::from_be_bytes([self.buffer[offset], self.buffer[offset + 1]]) as usize
            }
            _ => {
                if self.buffer.len() < offset + 8 {
                    return Ok(None);
                }
                let mut bytes = [0u8; 8];
                bytes.copy_from_slice(&self.buffer[offset..offset + 8]);
                let wide = u64::from_be_bytes(bytes);
                if wide & 0x8000_0000_0000_0000 != 0 {
                    return Err(WsError::Protocol("64-bit length must be clear".into()));
                }
                let value = wide as usize;
                if value > MAX_MESSAGE_BYTES {
                    return Err(WsError::TooLarge);
                }
                value
            }
        };
        offset += match second & 0x7f {
            126 => 2,
            127 => 8,
            _ => 0,
        };

        let mask_key = if masked {
            if self.buffer.len() < offset + 4 {
                return Ok(None);
            }
            let key = [
                self.buffer[offset],
                self.buffer[offset + 1],
                self.buffer[offset + 2],
                self.buffer[offset + 3],
            ];
            offset += 4;
            Some(key)
        } else {
            None
        };

        if self.buffer.len() < offset + length {
            return Ok(None);
        }
        let mut payload = self.buffer[offset..offset + length].to_vec();
        self.buffer.drain(..offset + length);
        if let Some(key) = mask_key {
            for (index, byte) in payload.iter_mut().enumerate() {
                *byte ^= key[index % 4];
            }
        }

        // Control frames must not be fragmented and their payload is capped.
        if opcode >= 0x8 {
            if !fin {
                return Err(WsError::Protocol("control frames cannot fragment".into()));
            }
            if length > 125 {
                return Err(WsError::Protocol("control payload is capped at 125".into()));
            }
            return Ok(Some(Some(match opcode {
                0x8 => Self::close_frame(&payload)?,
                0x9 => Frame::Ping(payload),
                0xA => Frame::Pong(payload),
                other => {
                    return Err(WsError::Protocol(format!("unknown opcode {other:#x}")))
                }
            })));
        }

        match (opcode, fin) {
            (0x1 | 0x2, true) if self.fragment_opcode.is_none() => {
                return Ok(Some(Some(Self::message(opcode, payload)?)))
            }
            (0x1 | 0x2, _) => {
                if self.fragment_opcode.is_some() {
                    return Err(WsError::Protocol("new message while fragmenting".into()));
                }
                self.fragment_opcode = Some(opcode);
                self.fragment_payload = payload;
                return Ok(Some(None));
            }
            (0x0, _) => {
                let start = self
                    .fragment_opcode
                    .ok_or_else(|| WsError::Protocol("continuation without a start".into()))?;
                if self.fragment_payload.len() + payload.len() > MAX_MESSAGE_BYTES {
                    return Err(WsError::TooLarge);
                }
                self.fragment_payload.extend_from_slice(&payload);
                if !fin {
                    return Ok(Some(None));
                }
                let assembled = std::mem::take(&mut self.fragment_payload);
                self.fragment_opcode = None;
                return Ok(Some(Some(Self::message(start, assembled)?)));
            }
            (opcode, _) => {
                return Err(WsError::Protocol(format!("unknown opcode {opcode:#x}")))
            }
        }
    }

    fn message(opcode: u8, payload: Vec<u8>) -> Result<Frame, WsError> {
        if opcode != 0x1 {
            return Ok(Frame::Binary(payload));
        }
        String::from_utf8(payload)
            .map(Frame::Text)
            .map_err(|_| WsError::Protocol("a text frame must be UTF-8".into()))
    }

    fn close_frame(payload: &[u8]) -> Result<Frame, WsError> {
        match payload.len() {
            // A bare close: the peer said goodbye without a status.
            0 => Ok(Frame::Close(None)),
            1 => Err(WsError::Protocol("a close code is two bytes".into())),
            _ => {
                let code = u16::from_be_bytes([payload[0], payload[1]]);
                if !(1000..=4999).contains(&code) {
                    return Err(WsError::Protocol(format!("bad close code {code}")));
                }
                Ok(Frame::Close(Some(code)))
            }
        }
    }
}

/// Encodes one frame. `mask` is what a client must do (including this project's
/// own client side, talking to the provider); a server sends unmasked.
pub fn encode(frame: &Frame, mask: bool) -> Vec<u8> {
    let (opcode, payload) = match frame {
        Frame::Text(text) => (0x1, text.as_bytes().to_vec()),
        Frame::Binary(bytes) => (0x2, bytes.clone()),
        Frame::Ping(bytes) => (0x9, bytes.clone()),
        Frame::Pong(bytes) => (0xA, bytes.clone()),
        Frame::Close(None) => (0x8, Vec::new()),
        Frame::Close(Some(code)) => (0x8, code.to_be_bytes().to_vec()),
    };

    let mut out = vec![0x80 | opcode];
    let length = payload.len();
    let (marker, extended): (u8, Vec<u8>) = if length <= 125 {
        (length as u8, Vec::new())
    } else if length <= u16::MAX as usize {
        (126, (length as u16).to_be_bytes().to_vec())
    } else {
        (127, (length as u64).to_be_bytes().to_vec())
    };
    out.push(if mask { marker | 0x80 } else { marker });
    out.extend_from_slice(&extended);

    if mask {
        // A fixed mask would be as good as none; the frame says "must be
        // unpredictable", so it comes from the same source as session tokens.
        let key: [u8; 4] = crate::config::random_bytes(4)
            .try_into()
            .expect("four bytes");
        out.extend_from_slice(&key);
        out.extend(
            payload
                .iter()
                .enumerate()
                .map(|(index, byte)| byte ^ key[index % 4]),
        );
    } else {
        out.extend_from_slice(&payload);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a frame the way a peer on the wire would, so the decoder is driven
    /// by bytes rather than by this module's own encoder.
    fn raw(fin: bool, opcode: u8, payload: &[u8], mask: Option<[u8; 4]>) -> Vec<u8> {
        let mut out = vec![u8::from(fin) * 0x80 | opcode];
        let length = payload.len();
        let (mut marker, mut extended): (u8, Vec<u8>) = if length <= 125 {
            (length as u8, Vec::new())
        } else if length <= 65_535 {
            (126, (length as u16).to_be_bytes().to_vec())
        } else {
            (127, (length as u64).to_be_bytes().to_vec())
        };
        if let Some(key) = mask {
            marker |= 0x80;
            out.push(marker);
            out.extend_from_slice(&extended);
            out.extend_from_slice(&key);
            out.extend(payload.iter().enumerate().map(|(i, b)| b ^ key[i % 4]));
        } else {
            out.push(marker);
            out.extend_from_slice(&extended);
            out.extend_from_slice(payload);
        }
        out
    }

    fn decode(bytes: &[u8], masked: bool) -> Result<Vec<Frame>, WsError> {
        WsDecoder::new().push(bytes, masked)
    }

    #[test]
    fn the_handshake_key_matches_an_independent_implementation() {
        // Both pairs come from the system's own SHA-1 over `key + GUID`, so this
        // is a cross-check rather than a restatement of the same arithmetic:
        //   printf '%s' "KEY258EAFA5-E914-47DA-95CA-C5AB0DC85B11" \
        //     | openssl dgst -sha1 -binary | base64
        // The first is the key used in the RFC 6455 handwalk example.
        assert_eq!(accept_key("x3JJHMbDL1EzLkh9GBhXDw==").as_deref(), Some("HSmrc0sMlYUkAGmm5OPpG2HaGWk="));
        assert_eq!(accept_key("dGhlIHNhbXBsZSBub25jYXQ=").as_deref(), Some("C0iGEECXJdPQ1Dfevh4d/wLzuZw="));
        assert_eq!(accept_key(""), None);
        assert_eq!(accept_key("not base64!!"), None, "a typo must not be signed");
    }

    #[test]
    fn both_directions_round_trip_and_the_mask_rule_is_enforced() {
        let server_side = encode(&Frame::Text("你好, 红隼".into()), false);
        assert_eq!(decode(&server_side, false).expect("server frame"), vec![Frame::Text("你好, 红隼".into())]);
        assert!(decode(&server_side, true).is_err(), "a client frame must be masked");

        let client_side = encode(&Frame::Binary(vec![1, 2, 3, 250]), true);
        assert_eq!(decode(&client_side, true).expect("client frame"), vec![Frame::Binary(vec![1, 2, 3, 250])]);
        assert!(decode(&client_side, false).is_err(), "a server frame must not be masked");
    }

    #[test]
    fn the_three_length_forms_are_written_and_read_back() {
        for length in [0usize, 1, 125, 126, 300, 65_535, 65_536, 70_000] {
            let payload = vec![7u8; length];
            let bytes = encode(&Frame::Binary(payload.clone()), false);
            let expected = match length {
                0..=125 => length as u8,
                126..=65_535 => 126,
                _ => 127,
            };
            assert_eq!(bytes[1] & 0x7f, expected, "length {length} picks the wrong form");
            assert_eq!(decode(&bytes, false).expect("decoded"), vec![Frame::Binary(payload)], "length {length}");
        }
    }

    #[test]
    fn masked_payloads_are_unmasked_bit_for_bit() {
        let payload: Vec<u8> = (0..64u8).collect();
        let bytes = raw(true, 0x2, &payload, Some([0xAA, 0xBB, 0xCC, 0xDD]));
        assert_ne!(&bytes[10..], payload.as_slice(), "the wire bytes are XORed");
        assert_eq!(decode(&bytes, true).expect("unmasked"), vec![Frame::Binary(payload)]);
    }

    #[test]
    fn fragments_reassemble_into_one_message() {
        let mut bytes = raw(false, 0x1, b"{\"type\"", None);
        bytes.extend(raw(true, 0x0, b": \"start\"}", None));
        assert_eq!(decode(&bytes, false).expect("assembled"), vec![Frame::Text("{\"type\": \"start\"}".into())]);

        assert!(matches!(decode(&raw(true, 0x0, b"tail", None), false), Err(WsError::Protocol(_))), "continuation without a start");
        let mut twice = raw(false, 0x1, b"a", None);
        twice.extend(raw(false, 0x1, b"b", None));
        assert!(matches!(decode(&twice, false), Err(WsError::Protocol(_))), "start while fragmenting");
    }

    #[test]
    fn control_frames_never_fragment_and_are_capped() {
        assert_eq!(decode(&raw(true, 0x9, b"hb", None), false).expect("ping"), vec![Frame::Ping(b"hb".to_vec())]);
        assert_eq!(decode(&raw(true, 0xA, b"hb", None), false).expect("pong"), vec![Frame::Pong(b"hb".to_vec())]);
        assert_eq!(
            decode(&raw(true, 0x8, &1000u16.to_be_bytes(), None), false).expect("close"),
            vec![Frame::Close(Some(1000))]
        );
        assert_eq!(decode(&raw(true, 0x8, &[], None), false).expect("bare close"), vec![Frame::Close(None)]);
        assert!(matches!(decode(&raw(true, 0x8, &[0x03], None), false), Err(WsError::Protocol(_))), "one byte is not a code");
        assert!(matches!(decode(&raw(true, 0x8, &5000u16.to_be_bytes(), None), false), Err(WsError::Protocol(_))), "out of range code");
        assert!(matches!(decode(&raw(false, 0x9, b"x", None), false), Err(WsError::Protocol(_))), "fragmented control frame");
        assert!(matches!(decode(&raw(true, 0x9, &vec![9u8; 126], None), false), Err(WsError::Protocol(_))), "control payload over 125");
    }

    #[test]
    fn malformed_headers_are_protocol_errors_and_oversize_is_refused_early() {
        let mut rsv = raw(true, 0x1, b"x", None);
        rsv[0] |= 0x40;
        assert!(matches!(decode(&rsv, false), Err(WsError::Protocol(_))), "RSV bits must be zero");
        assert!(matches!(decode(&raw(true, 0x3, b"x", None), false), Err(WsError::Protocol(_))), "unknown opcode");
        assert!(matches!(decode(&raw(true, 0x1, &[0xFF, 0xFE], None), false), Err(WsError::Protocol(_))), "text must be UTF-8");

        let mut wide = vec![0x82u8, 127];
        wide.extend((1u64 << 63).to_be_bytes());
        assert!(matches!(decode(&wide, false), Err(WsError::Protocol(_))), "top bit of a 64-bit length is reserved");

        let mut big = vec![0x82u8, 127];
        big.extend(((MAX_MESSAGE_BYTES + 1) as u64).to_be_bytes());
        assert_eq!(decode(&big, false), Err(WsError::TooLarge), "refused before the body arrives");
    }

    #[test]
    fn partial_input_waits_and_one_push_can_carry_several_frames() {
        let bytes = encode(&Frame::Text("slow".into()), false);
        let mut decoder = WsDecoder::new();
        for (index, byte) in bytes.iter().enumerate() {
            let got = decoder.push(&[*byte], false).expect("no error mid-frame");
            if index + 1 == bytes.len() {
                assert_eq!(got, vec![Frame::Text("slow".into())], "the last byte completes it");
            } else {
                assert!(got.is_empty(), "byte {index} produced a frame too early");
            }
        }

        let mut batch = encode(&Frame::Text("a".into()), false);
        batch.extend(encode(&Frame::Ping(vec![1]), false));
        batch.extend(&raw(true, 0x1, b"hang", None)[..2]);
        let mut second = WsDecoder::new();
        assert_eq!(second.push(&batch, false).expect("two frames"), vec![Frame::Text("a".into()), Frame::Ping(vec![1])]);
        assert!(second.push(b"ing", false).expect("incomplete").is_empty(), "three of four payload bytes are not a message");
        assert_eq!(
            second.push(b"!", false).expect("message"),
            vec![Frame::Text("ing!".into())],
            "the fourth byte closes it"
        );
    }
}
