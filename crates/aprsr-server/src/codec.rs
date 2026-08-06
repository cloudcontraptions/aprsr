//! Line framing for APRS-IS connections.
//!
//! This exists instead of `tokio_util::codec::LinesCodec` for one reason: that codec
//! signals an over-long line by returning an *error*, and `FramedRead` yields `None` on
//! the poll after any decoder error. A connection driven by a `while let Some(...)` loop
//! therefore ends the moment a client sends one oversized packet — which is a client bug,
//! not grounds for disconnection.
//!
//! [`LineCodec`] reports the same conditions as ordinary items instead, so the stream only
//! ends when the socket does. It enforces the 512-byte limit from
//! <http://www.aprs-is.net/Connecting.aspx> while decoding, so an endless line without a
//! newline cannot exhaust memory.

use std::cmp;
use std::io;

use tokio_util::bytes::{Buf, BytesMut};
use tokio_util::codec::Decoder;

/// One decoded line, or the reason it could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    /// A complete line, with its CR/LF removed.
    Text(String),
    /// The line exceeded the length limit and was discarded.
    Oversized,
    /// The line was not valid UTF-8 and was discarded.
    ///
    /// APRS payloads are historically byte-transparent rather than UTF-8, so this is a
    /// real limitation rather than purely a defence against malformed input; see
    /// `docs/roadmap.md`.
    NotUtf8,
}

/// Splits a byte stream into APRS-IS lines.
#[derive(Debug, Clone)]
pub struct LineCodec {
    max_length: usize,
    /// True while discarding the tail of an over-long line.
    discarding: bool,
    /// How far into the buffer has already been scanned for a newline.
    next_index: usize,
}

impl LineCodec {
    /// A codec enforcing the APRS-IS maximum line length.
    #[must_use]
    pub const fn new() -> Self {
        Self::with_max_length(aprsr_core::packet::MAX_PACKET_LEN)
    }

    #[must_use]
    pub const fn with_max_length(max_length: usize) -> Self {
        Self {
            max_length,
            discarding: false,
            next_index: 0,
        }
    }
}

impl Default for LineCodec {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder for LineCodec {
    type Item = Line;
    type Error = io::Error;

    fn decode(&mut self, buf: &mut BytesMut) -> Result<Option<Line>, io::Error> {
        loop {
            // Never scan further than one byte past the limit: that is enough to know the
            // line is too long, and it bounds the work done per call.
            let read_to = cmp::min(self.max_length.saturating_add(1), buf.len());
            let newline = buf
                .get(self.next_index..read_to)
                .and_then(|window| window.iter().position(|b| *b == b'\n'));

            match (self.discarding, newline) {
                // The end of an over-long line: drop everything up to and including it.
                (true, Some(offset)) => {
                    buf.advance(offset + self.next_index + 1);
                    self.discarding = false;
                    self.next_index = 0;
                }
                // Still inside an over-long line.
                (true, None) => {
                    buf.advance(read_to);
                    self.next_index = 0;
                    if buf.is_empty() {
                        return Ok(None);
                    }
                }
                // A complete line.
                (false, Some(offset)) => {
                    let newline_index = offset + self.next_index;
                    self.next_index = 0;
                    let line = buf.split_to(newline_index + 1);
                    let line = line.get(..line.len().saturating_sub(1)).unwrap_or_default();
                    return Ok(Some(decode_line(strip_carriage_return(line))));
                }
                // No newline yet, and the line has already run past the limit.
                (false, None) if buf.len() > self.max_length => {
                    self.discarding = true;
                    return Ok(Some(Line::Oversized));
                }
                // No newline yet; wait for more bytes.
                (false, None) => {
                    self.next_index = buf.len();
                    return Ok(None);
                }
            }
        }
    }

    fn decode_eof(&mut self, buf: &mut BytesMut) -> Result<Option<Line>, io::Error> {
        if let Some(line) = self.decode(buf)? {
            return Ok(Some(line));
        }
        if buf.is_empty() {
            return Ok(None);
        }
        // A client that closed without a trailing newline still sent a line.
        let line = buf.split_to(buf.len());
        self.next_index = 0;
        Ok(Some(decode_line(strip_carriage_return(&line))))
    }
}

fn decode_line(bytes: &[u8]) -> Line {
    match std::str::from_utf8(bytes) {
        Ok(text) => Line::Text(text.to_owned()),
        Err(_) => Line::NotUtf8,
    }
}

fn strip_carriage_return(bytes: &[u8]) -> &[u8] {
    match bytes.split_last() {
        Some((b'\r', head)) => head,
        _ => bytes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn feed(codec: &mut LineCodec, data: &[u8]) -> Vec<Line> {
        let mut buf = BytesMut::from(data);
        let mut out = Vec::new();
        while let Ok(Some(line)) = codec.decode(&mut buf) {
            out.push(line);
        }
        out
    }

    #[rstest]
    #[case(b"hello\n", "hello")]
    #[case(b"hello\r\n", "hello")] // APRS-IS terminates with CRLF
    #[case(b"\n", "")] // an empty line is still a line
    fn decodes_one_line(#[case] input: &[u8], #[case] expected: &str) {
        let mut codec = LineCodec::new();
        assert_eq!(feed(&mut codec, input), [Line::Text(expected.to_owned())]);
    }

    #[test]
    fn decodes_several_lines_from_one_buffer() {
        let mut codec = LineCodec::new();
        assert_eq!(
            feed(&mut codec, b"one\r\ntwo\r\nthree\r\n"),
            [
                Line::Text("one".to_owned()),
                Line::Text("two".to_owned()),
                Line::Text("three".to_owned()),
            ]
        );
    }

    #[test]
    fn waits_for_a_line_split_across_reads() {
        let mut codec = LineCodec::new();
        let mut buf = BytesMut::from(&b"partial"[..]);
        assert_eq!(codec.decode(&mut buf).expect("no error"), None);

        buf.extend_from_slice(b" line\r\n");
        assert_eq!(
            codec.decode(&mut buf).expect("no error"),
            Some(Line::Text("partial line".to_owned()))
        );
    }

    /// The behaviour this codec exists for: an over-long line is reported and skipped, and
    /// the next line still decodes.
    #[test]
    fn an_oversized_line_is_reported_and_the_stream_continues() {
        let mut codec = LineCodec::with_max_length(16);
        let mut input = vec![b'x'; 64];
        input.extend_from_slice(b"\r\nnext\r\n");

        assert_eq!(
            feed(&mut codec, &input),
            [Line::Oversized, Line::Text("next".to_owned())]
        );
    }

    #[test]
    fn a_line_exactly_at_the_limit_is_accepted() {
        let mut codec = LineCodec::with_max_length(16);
        let mut input = vec![b'x'; 16];
        input.push(b'\n');
        assert_eq!(feed(&mut codec, &input), [Line::Text("x".repeat(16))]);
    }

    #[test]
    fn an_oversized_line_split_across_reads_is_still_skipped() {
        let mut codec = LineCodec::with_max_length(8);
        let mut buf = BytesMut::from(&b"aaaaaaaaaaaaaaaa"[..]);
        assert_eq!(
            codec.decode(&mut buf).expect("no error"),
            Some(Line::Oversized)
        );

        buf.extend_from_slice(b"aaaa\r\ngood\r\n");
        let mut seen = Vec::new();
        while let Ok(Some(line)) = codec.decode(&mut buf) {
            seen.push(line);
        }
        assert_eq!(seen, [Line::Text("good".to_owned())]);
    }

    /// A packet with non-UTF-8 bytes is reported rather than killing the connection.
    #[test]
    fn invalid_utf8_is_reported_as_such() {
        let mut codec = LineCodec::new();
        assert_eq!(
            feed(&mut codec, &[0xff, 0xfe, b'\r', b'\n', b'o', b'k', b'\n']),
            [Line::NotUtf8, Line::Text("ok".to_owned())]
        );
    }

    #[test]
    fn a_final_line_without_a_newline_is_returned_at_eof() {
        let mut codec = LineCodec::new();
        let mut buf = BytesMut::from(&b"no trailing newline"[..]);
        assert_eq!(
            codec.decode_eof(&mut buf).expect("no error"),
            Some(Line::Text("no trailing newline".to_owned()))
        );
        assert_eq!(codec.decode_eof(&mut buf).expect("no error"), None);
    }

    #[test]
    fn eof_on_an_empty_buffer_ends_the_stream() {
        let mut codec = LineCodec::new();
        let mut buf = BytesMut::new();
        assert_eq!(codec.decode_eof(&mut buf).expect("no error"), None);
    }

    #[test]
    fn the_default_limit_matches_the_specification() {
        let mut codec = LineCodec::new();
        let mut input = vec![b'x'; aprsr_core::packet::MAX_PACKET_LEN + 1];
        input.push(b'\n');
        assert_eq!(feed(&mut codec, &input), [Line::Oversized]);
    }
}
