//! Server-sent events over any [`AsyncRead`], in the one shape a chat completion
//! streams in: `data:` lines, an event ended by a blank line, and a literal
//! `data: [DONE]` for the end.
//!
//! Two things the wire forces, and a line-at-a-time reader would get both
//! wrong. Bytes arrive in whatever sizes the socket hands over, so the split
//! into lines is done here rather than trusted to one `read`. And a multibyte
//! character can land with its bytes divided between two frames — a gateway
//! has been seen to do it — so a line is never turned into text until its
//! newline has arrived, and the tail that is not yet a line is held as raw
//! bytes until the rest of it comes. That is the whole trick: a `\n` is a
//! byte no multibyte character contains, so a complete line is always
//! complete UTF-8, and anything else is not decoded at all.

use std::io::{Error, ErrorKind};
use tokio::io::{AsyncRead, AsyncReadExt};

/// Reads one event's joined `data` at a time off a byte stream.
pub struct SseReader<R: AsyncRead + Unpin> {
    inner: R,
    /// Bytes read but not yet formed into whole lines. A half-arrived
    /// character lives here, never turned into text, until the rest of it
    /// arrives.
    buf: Vec<u8>,
    /// Retained across cancelled next_event futures until a separator arrives.
    data: Vec<String>,
    /// The stream has ended; what is left in the buffer is being drained.
    eof: bool,
    /// `data: [DONE]` was seen; every further call answers `None`.
    done: bool,
}

impl<R: AsyncRead + Unpin> SseReader<R> {
    #[must_use]
    pub fn new(inner: R) -> SseReader<R> {
        SseReader {
            inner,
            buf: Vec::new(),
            data: Vec::new(),
            eof: false,
            done: false,
        }
    }

    /// The next event's `data`, its lines joined with `"\n"`; `Ok(None)` at
    /// the end of the stream, and forever after `data: [DONE]`.
    ///
    /// A `:` comment, an `event:`, an `id:` or a `retry:` is read and
    /// dropped: this stream names none of them, and a parser that stopped at
    /// one would stop at a keep-alive. A last event the stream never closed
    /// with a blank line is still delivered — the peer's manners are not the
    /// answer's problem.
    /// Cancelling this future retains every partially received event.
    ///
    /// # Errors
    ///
    /// If a read of the underlying stream fails, or a whole line of it is not
    /// UTF-8 — which is [`ErrorKind::InvalidData`], not a lossy replacement:
    /// a stream carrying JSON is either text or broken.
    pub async fn next_event(&mut self) -> std::io::Result<Option<String>> {
        if self.done {
            return Ok(None);
        }
        loop {
            while let Some(line) = self.take_line()? {
                if line.is_empty() {
                    // A blank line ends an event. One that carried no `data`
                    // at all — a stray comment, a lone `event:` — is only a
                    // separator, so keep reading.
                    if !self.data.is_empty() {
                        return Ok(Some(std::mem::take(&mut self.data).join("\n")));
                    }
                    continue;
                }
                if let Some(rest) = line.strip_prefix("data:") {
                    // SSE drops one optional space after the colon, and only
                    // one: the rest is the value.
                    let value = rest.strip_prefix(' ').unwrap_or(rest);
                    if value == "[DONE]" {
                        self.done = true;
                        return Ok(None);
                    }
                    self.data.push(value.to_string());
                }
            }
            if self.eof {
                return Ok(
                    (!self.data.is_empty()).then(|| std::mem::take(&mut self.data).join("\n"))
                );
            }
            if !self.fill().await? {
                // The end of the stream. Let a last line with no newline
                // resolve as a line, and drain it on the next turn.
                self.eof = true;
                if !self.buf.is_empty() {
                    self.buf.push(b'\n');
                }
            }
        }
    }

    /// The next complete line — the bytes up to and not including a `\n`, a
    /// trailing `\r` dropped — or `None` while the buffer holds no whole line
    /// yet.
    fn take_line(&mut self) -> std::io::Result<Option<String>> {
        let Some(nl) = self.buf.iter().position(|&b| b == b'\n') else {
            return Ok(None);
        };
        let mut line: Vec<u8> = self.buf.drain(..=nl).collect();
        line.pop(); // the '\n'
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        String::from_utf8(line)
            .map(Some)
            .map_err(|_| Error::new(ErrorKind::InvalidData, "a line of the stream is not utf-8"))
    }

    /// Reads one more piece onto the buffer. `Ok(false)` at the end of the
    /// stream.
    async fn fill(&mut self) -> std::io::Result<bool> {
        let mut tmp = [0u8; 4096];
        loop {
            return match self.inner.read(&mut tmp).await {
                Ok(0) => Ok(false),
                Ok(n) => {
                    self.buf.extend_from_slice(&tmp[..n]);
                    Ok(true)
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => Err(e),
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// A reader that hands over the frames it is given, one a call, so a test
    /// can place a split exactly where it wants one.
    struct Frames {
        frames: Vec<Vec<u8>>,
    }

    impl Frames {
        fn new(frames: &[&[u8]]) -> Frames {
            let mut frames: Vec<Vec<u8>> = frames.iter().map(|f| f.to_vec()).collect();
            frames.reverse();
            Frames { frames }
        }

        /// Every byte its own frame: no single read is a whole line, so the
        /// parser has to do the splitting itself.
        fn byte_at_a_time(bytes: &[u8]) -> Frames {
            Frames {
                frames: bytes.iter().rev().map(|b| vec![*b]).collect(),
            }
        }
    }

    impl AsyncRead for Frames {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            let Some(front) = self.frames.last_mut() else {
                return std::task::Poll::Ready(Ok(()));
            };
            let n = front.len().min(buf.remaining());
            buf.put_slice(&front[..n]);
            front.drain(..n);
            if front.is_empty() {
                self.frames.pop();
            }
            std::task::Poll::Ready(Ok(()))
        }
    }

    /// Every event of a stream given whole, for the cases where the framing
    /// and not the chunking is what is being read.
    async fn events(raw: &[u8]) -> Vec<String> {
        let mut sse = SseReader::new(Cursor::new(raw.to_vec()));
        let mut out = Vec::new();
        while let Some(e) = sse.next_event().await.unwrap() {
            out.push(e);
        }
        out
    }

    #[tokio::test(start_paused = true)]
    async fn cancelling_a_wait_keeps_the_partial_event() {
        use tokio::io::AsyncWriteExt;
        let (mut writer, reader) = tokio::io::duplex(128);
        let mut sse = SseReader::new(reader);
        writer.write_all(b"data: first\n").await.unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), sse.next_event())
                .await
                .is_err()
        );
        writer.write_all(b"data: second\n\n").await.unwrap();
        assert_eq!(
            sse.next_event().await.unwrap().as_deref(),
            Some("first\nsecond")
        );
    }

    #[tokio::test]
    async fn one_event_comes_back_per_data_line() {
        assert_eq!(events(b"data: one\n\ndata: two\n\n").await, ["one", "two"]);
    }

    #[tokio::test]
    async fn multi_line_data_joins_with_newlines() {
        assert_eq!(
            events(b"data: first\ndata: second\n\n").await,
            ["first\nsecond"]
        );
    }

    #[tokio::test]
    async fn crlf_endings_are_taken_the_same_way() {
        assert_eq!(
            events(b"data: one\r\n\r\ndata: two\r\n\r\n").await,
            ["one", "two"]
        );
    }

    #[tokio::test]
    async fn comments_and_the_other_fields_are_ignored() {
        let raw = b": keep-alive\nevent: message\nid: 7\nretry: 3000\ndata: {\"a\":1}\n\n";
        assert_eq!(events(raw).await, [r#"{"a":1}"#]);
        // A blank line after nothing but a comment is a separator, not an
        // empty event.
        assert_eq!(events(b": ping\n\ndata: after\n\n").await, ["after"]);
    }

    #[tokio::test]
    async fn data_with_no_space_after_the_colon_is_the_same_data() {
        assert_eq!(
            events(b"data:tight\n\ndata:  wide\n\n").await,
            ["tight", " wide"]
        );
    }

    #[tokio::test]
    async fn done_ends_the_stream_and_it_stays_ended() {
        let mut sse = SseReader::new(Cursor::new(b"data: one\n\ndata: [DONE]\n\ndata: two\n\n"));
        assert_eq!(sse.next_event().await.unwrap().as_deref(), Some("one"));
        assert_eq!(sse.next_event().await.unwrap(), None);
        assert_eq!(sse.next_event().await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_last_event_with_no_blank_line_is_still_delivered() {
        let mut sse = SseReader::new(Cursor::new(b"data: one\n\ndata: cut off"));
        assert_eq!(sse.next_event().await.unwrap().as_deref(), Some("one"));
        assert_eq!(sse.next_event().await.unwrap().as_deref(), Some("cut off"));
        assert_eq!(sse.next_event().await.unwrap(), None);
    }

    #[tokio::test]
    async fn an_event_reassembles_across_reads_of_one_byte() {
        let mut sse = SseReader::new(Frames::byte_at_a_time(b"data: hello world\n\n"));
        assert_eq!(
            sse.next_event().await.unwrap().as_deref(),
            Some("hello world")
        );
        assert_eq!(sse.next_event().await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_character_split_across_reads_is_made_whole() {
        // "café": the two bytes of `é` divided between two frames. A lossy
        // decode of the first frame alone would leave a replacement
        // character behind.
        let mut sse = SseReader::new(Frames::new(&[b"data: caf\xC3", b"\xA9\n\n"]));
        assert_eq!(sse.next_event().await.unwrap().as_deref(), Some("café"));

        // The same character, one byte a read.
        let mut sse = SseReader::new(Frames::byte_at_a_time("data: café\n\n".as_bytes()));
        assert_eq!(sse.next_event().await.unwrap().as_deref(), Some("café"));

        // A four-byte character with the split inside it, delivered as two
        // slices — the shape `Read::chain` makes.
        let mut sse = SseReader::new(
            Cursor::new(b"data: hi \xF0\x9F".to_vec()).chain(Cursor::new(b"\x99\x82\n\n".to_vec())),
        );
        assert_eq!(sse.next_event().await.unwrap().as_deref(), Some("hi 🙂"));

        // And one byte a read, which puts a split at every one of its bytes.
        let mut sse = SseReader::new(Frames::byte_at_a_time("data: hi 🙂\n\n".as_bytes()));
        assert_eq!(sse.next_event().await.unwrap().as_deref(), Some("hi 🙂"));
    }

    #[tokio::test]
    async fn a_whole_line_that_is_not_utf8_is_an_error() {
        // 0xFF begins no character at all: this line will never become text,
        // and saying so is better than handing on a replacement character in
        // the middle of someone's JSON.
        let mut sse = SseReader::new(Cursor::new(b"data: broken \xFF\n\n".to_vec()));
        let e = sse.next_event().await.unwrap_err();
        assert_eq!(e.kind(), ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn an_answer_arrives_the_way_a_gateway_sends_one() {
        // Frames as a stream is really cut: two events in one, an event
        // split across two, a keep-alive comment, the terminator.
        let mut sse = SseReader::new(Frames::new(&[
            b"data: {\"delta\":\"Hel\"}\n\ndata: {\"delta\":\"lo\"}\n\n: ping\n\ndata: {\"del",
            b"ta\":\" there\"}\n",
            b"\ndata: [DONE]\n\n",
        ]));
        assert_eq!(
            sse.next_event().await.unwrap().as_deref(),
            Some(r#"{"delta":"Hel"}"#)
        );
        assert_eq!(
            sse.next_event().await.unwrap().as_deref(),
            Some(r#"{"delta":"lo"}"#)
        );
        assert_eq!(
            sse.next_event().await.unwrap().as_deref(),
            Some(r#"{"delta":" there"}"#)
        );
        assert_eq!(sse.next_event().await.unwrap(), None);
    }
}
