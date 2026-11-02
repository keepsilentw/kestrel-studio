/// Splits a byte stream into SSE frames.
///
/// Frames are separated by a blank line. The Bailian endpoint additionally emits
/// `id:N` lines and a non-standard `:HTTP_STATUS/200` comment line per frame,
/// both of which are tolerated here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseFrame {
    pub event: Option<String>,
    pub data: String,
}

#[derive(Default)]
pub struct SseParser {
    buffer: String,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds one chunk and returns every frame the buffer completed.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<SseFrame> {
        self.buffer.push_str(&String::from_utf8_lossy(chunk));
        let mut frames = Vec::new();
        while let Some(index) = self.buffer.find("\n\n") {
            let raw: String = self.buffer[..index].to_string();
            self.buffer.drain(..index + 2);
            if let Some(frame) = parse_frame(&raw) {
                frames.push(frame);
            }
        }
        frames
    }

    /// The trailing partial frame, if any, once the stream ended.
    pub fn finish(&mut self) -> Option<SseFrame> {
        let rest = std::mem::take(&mut self.buffer);
        parse_frame(&rest)
    }
}

fn parse_frame(frame: &str) -> Option<SseFrame> {
    let mut event: Option<String> = None;
    let mut data_lines: Vec<String> = Vec::new();

    for line in frame.split('\n') {
        if line.is_empty() || line.starts_with(':') {
            // Comment / keep-alive line. The Bailian endpoint also emits
            // `:HTTP_STATUS/200` here, which is why this is skipped rather than
            // parsed.
            continue;
        }
        if let Some(rest) = line.strip_prefix("event:") {
            event = Some(rest.trim().to_string());
            continue;
        }
        if let Some(rest) = line.strip_prefix("data:") {
            // Only one leading space is significant per the SSE spec.
            data_lines.push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
        }
    }

    if event.is_none() && data_lines.is_empty() {
        return None;
    }
    Some(SseFrame {
        event,
        data: data_lines.join("\n"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_standard_comment_and_id_lines_are_tolerated() {
        let mut parser = SseParser::new();
        let frames = parser.push(
            b":HTTP_STATUS/200\nid: 42\nevent: response.output_text.delta\ndata: {\"delta\":\"hi\"}\n\n",
        );
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].event.as_deref(), Some("response.output_text.delta"));
        assert_eq!(frames[0].data, "{\"delta\":\"hi\"}");
    }

    #[test]
    fn a_frame_split_across_chunks_is_reassembled() {
        let mut parser = SseParser::new();
        assert!(parser.push(b"event: text\nda").is_empty());
        let frames = parser.push(b"ta: one\ndata: two\n\n");
        assert_eq!(frames.len(), 1);
        // A multi-line value is several `data:` lines, joined with a newline; a
        // bare line carries no field and is ignored.
        assert_eq!(frames[0].event.as_deref(), Some("text"));
        assert_eq!(frames[0].data, "one\ntwo");
    }

    #[test]
    fn a_trailing_frame_without_a_blank_line_still_arrives_at_finish() {
        let mut parser = SseParser::new();
        assert!(parser.push(b"event: text\ndata: tail").is_empty());
        let frame = parser.finish().expect("trailing frame");
        assert_eq!(frame.event.as_deref(), Some("text"));
        assert_eq!(frame.data, "tail");
        assert!(parser.finish().is_none(), "the buffer is drained");
    }

    #[test]
    fn comment_only_frames_are_dropped() {
        let mut parser = SseParser::new();
        assert!(parser.push(b": ping\n\n").is_empty());
    }

    #[test]
    fn exactly_one_leading_space_after_the_colon_is_stripped() {
        let frame = parse_frame("data:  two spaces").expect("frame");
        assert_eq!(frame.data, " two spaces");
    }
}
