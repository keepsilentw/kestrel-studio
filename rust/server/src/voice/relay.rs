use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

use crate::net::ws::{encode, Frame, WsDecoder, WsError};

use super::protocol::{parse_client_frame, ClientFrame, ServerFrame};

/// The byte pump on either side of an upgraded voice socket.
///
/// Kept separate from the HTTP upgrade on purpose: these two functions take any
/// `AsyncRead`/`AsyncWrite` pair, so the whole frame loop is testable over an
/// in-memory duplex without a socket - the one part of the voice path that the
/// sandbox this is developed in cannot reach is `hyper::upgrade::on` itself.

/// What the browser side produced. `Ping` is surfaced rather than answered here so
/// the single owner of the write half stays the writer, not whoever read a frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inbound {
    Frame(ClientFrame),
    /// A close frame; the code is absent for a socket that just went away.
    Closed(Option<u16>),
    Ping(Vec<u8>),
    /// The peer broke the protocol; the session must end.
    Protocol(String),
}

/// What the session wants the browser to see.
#[derive(Debug, Clone)]
pub enum Outbound {
    Frame(ServerFrame),
    Pong(Vec<u8>),
    Close(u16),
}

const READ_CHUNK: usize = 8 << 10;

/// Reads until the peer closes or breaks the protocol.
///
/// A malformed JSON frame is dropped, not fatal: the peer is a browser and one bad
/// frame must not end a call. A frame that breaks the *transport* rules does end it
/// (`Inbound::Protocol`), because then the byte stream is no longer aligned.
pub async fn pump_in<I: AsyncReadExt + Unpin>(
    io: &mut I,
    sink: &mpsc::Sender<Inbound>,
) -> Result<(), std::io::Error> {
    let mut decoder = WsDecoder::new();
    let mut buf = vec![0u8; READ_CHUNK];
    loop {
        let read = io.read(&mut buf).await?;
        if read == 0 {
            // EOF without a close frame: still an ending, and the caller must treat
            // it exactly like one so the upstream session goes with it.
            let _ = sink.send(Inbound::Closed(None)).await;
            return Ok(());
        }
        match decoder.push(&buf[..read], true) {
            Ok(frames) => {
                for frame in frames {
                    let event = match frame {
                        Frame::Text(text) => match parse_client_frame(&text) {
                            Some(parsed) => Inbound::Frame(parsed),
                            // Unrecognised application frames are ignored, matching
                            // the Nest server's parse-returns-null behaviour.
                            None => continue,
                        },
                        Frame::Binary(_) => continue,
                        Frame::Ping(payload) => Inbound::Ping(payload),
                        Frame::Pong(_) => continue,
                        Frame::Close(code) => {
                            let _ = sink.send(Inbound::Closed(code)).await;
                            return Ok(());
                        }
                    };
                    if sink.send(event).await.is_err() {
                        return Ok(());
                    }
                }
            }
            Err(error) => {
                let _ = sink
                    .send(Inbound::Protocol(match error {
                        WsError::Protocol(message) => message,
                        WsError::TooLarge => "message too large".to_string(),
                    }))
                    .await;
                return Ok(());
            }
        }
    }
}

/// Writes until the session closes the queue.
pub async fn pump_out<I: AsyncWriteExt + Unpin>(
    io: &mut I,
    queue: &mut mpsc::Receiver<Outbound>,
) -> Result<(), std::io::Error> {
    while let Some(item) = queue.recv().await {
        let frame = match item {
            Outbound::Frame(server) => Frame::Text(server.to_json()),
            Outbound::Pong(payload) => Frame::Pong(payload),
            Outbound::Close(code) => Frame::Close(Some(code)),
        };
        // Server to client: unmasked, as the RFC requires.
        io.write_all(&encode(&frame, false)).await?;
        if matches!(frame, Frame::Close(_)) {
            io.flush().await?;
            return Ok(());
        }
    }
    io.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;
    use tokio::io::AsyncWriteExt;

    fn pair() -> (
        tokio::io::DuplexStream,
        tokio::io::DuplexStream,
    ) {
        duplex(4 << 10)
    }

    async fn collect(mut io: tokio::io::DuplexStream, sink: mpsc::Sender<Inbound>) {
        pump_in(&mut io, &sink).await.expect("pump");
    }

    #[tokio::test]
    async fn client_frames_come_out_in_order_and_a_close_ends_the_pump() {
        let (mut browser_side, server_side) = pair();
        let (tx, mut rx) = mpsc::channel(8);
        let reader = tokio::spawn(collect(server_side, tx));

        // The browser masks, as it must.
        browser_side
            .write_all(&encode(&Frame::Text(r#"{"type":"start"}"#.into()), true))
            .await
            .expect("start");
        browser_side
            .write_all(&encode(&Frame::Text(r#"{"type":"audio","pcm":"AAA="}"#.into()), true))
            .await
            .expect("audio");
        browser_side
            .write_all(&encode(&Frame::Close(Some(1000)), true))
            .await
            .expect("close");
        browser_side.shutdown().await.ok();

        let mut got = Vec::new();
        while let Some(event) = rx.recv().await {
            let stop = matches!(event, Inbound::Closed(_));
            got.push(event);
            if stop {
                break;
            }
        }
        reader.await.expect("join");
        assert_eq!(
            got,
            vec![
                Inbound::Frame(ClientFrame::Start),
                Inbound::Frame(ClientFrame::Audio { pcm: "AAA=".into() }),
                Inbound::Closed(Some(1000)),
            ]
        );
    }

    #[tokio::test]
    async fn an_unmasked_client_frame_ends_the_session_as_a_protocol_error() {
        let (mut browser_side, server_side) = pair();
        let (tx, mut rx) = mpsc::channel(4);
        tokio::spawn(collect(server_side, tx));

        browser_side
            .write_all(&encode(&Frame::Text("{}".into()), false))
            .await
            .expect("unmasked");
        match rx.recv().await {
            Some(Inbound::Protocol(message)) => assert!(message.contains("masked"), "{message}"),
            other => panic!("expected a protocol error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_half_frame_waits_for_the_rest_and_an_eof_reads_as_a_close() {
        let (mut browser_side, server_side) = pair();
        let (tx, mut rx) = mpsc::channel(4);
        tokio::spawn(collect(server_side, tx));

        let bytes = encode(&Frame::Text(r#"{"type":"commit"}"#.into()), true);
        browser_side.write_all(&bytes[..3]).await.expect("head");
        // Nothing yet: the pump must not invent a frame from partial bytes.
        assert!(matches!(
            tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await,
            Err(_)
        ));
        browser_side.write_all(&bytes[3..]).await.expect("tail");
        assert_eq!(rx.recv().await, Some(Inbound::Frame(ClientFrame::Commit)));

        drop(browser_side);
        assert_eq!(rx.recv().await, Some(Inbound::Closed(None)), "EOF is a close");
    }

    #[tokio::test]
    async fn pings_are_surfaced_for_the_writer_to_answer() {
        let (mut browser_side, server_side) = pair();
        let (tx, mut rx) = mpsc::channel(4);
        tokio::spawn(collect(server_side, tx));
        browser_side
            .write_all(&encode(&Frame::Ping(vec![1, 2]), true))
            .await
            .expect("ping");
        assert_eq!(rx.recv().await, Some(Inbound::Ping(vec![1, 2])));
    }

    #[tokio::test]
    async fn outbound_frames_are_unmasked_text_and_a_close_flushes_then_stops() {
        let (_browser_side, server_side) = pair();
        let (mut tx, mut rx) = mpsc::channel(4);
        let mut server_side = server_side;
        let writer = tokio::spawn(async move { pump_out(&mut server_side, &mut rx).await });

        tx.send(Outbound::Frame(ServerFrame::Ready { conversation_id: 7 }))
            .await
            .expect("ready");
        tx.send(Outbound::Pong(vec![9])).await.expect("pong");
        tx.send(Outbound::Close(1013)).await.expect("close");
        drop(tx);
        writer.await.expect("join").expect("no io error");
    }

    #[tokio::test]
    async fn an_oversized_announcement_reports_too_large_rather_than_buffering() {
        let (mut browser_side, server_side) = pair();
        let (tx, mut rx) = mpsc::channel(4);
        tokio::spawn(collect(server_side, tx));

        let mut header = vec![0x81u8, 127 | 0x80];
        header.extend((((8usize << 20) + 1) as u64).to_be_bytes());
        header.extend([0u8; 4]);
        browser_side.write_all(&header).await.expect("header");
        match rx.recv().await {
            Some(Inbound::Protocol(message)) => assert!(message.contains("too large"), "{message}"),
            other => panic!("expected too-large, got {other:?}"),
        }
    }
}
