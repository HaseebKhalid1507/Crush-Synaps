//! LSP-style Content-Length-framed JSON-RPC over stdin/stdout.
//!
//! The Synaps process-extension protocol (v1): each message is
//! `Content-Length: N\r\n\r\n<N bytes of UTF-8 JSON>`. This module is the
//! transport — it knows nothing about hooks or compression.

use std::io::{BufRead, Read, Write};

/// Reject frames larger than this to bound memory against a malformed or
/// malicious Content-Length. Tool output is capped at 256 KiB upstream; 64 MiB
/// is generous headroom for the framed JSON envelope.
const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;
/// A header line longer than this is malformed.
const MAX_HEADER_LINE: u64 = 8 * 1024;
/// Most header lines in one frame.
const MAX_HEADERS: usize = 32;

fn invalid(msg: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, msg.into())
}

/// Read one Content-Length-framed JSON message. Returns `Ok(None)` on clean
/// EOF (peer closed the pipe), `Err` (`InvalidData`) on a malformed frame,
/// after which the next call reads on from where this one stopped. Memory
/// stays bounded by what actually arrived, whatever the header claims.
pub fn read_message<R: BufRead>(reader: &mut R) -> std::io::Result<Option<serde_json::Value>> {
    let mut content_length: Option<usize> = None;
    let mut line = Vec::new();
    let mut headers = 0;
    loop {
        line.clear();
        let n = reader.by_ref().take(MAX_HEADER_LINE).read_until(b'\n', &mut line)?;
        if n == 0 {
            if headers == 0 {
                return Ok(None); // EOF
            }
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "EOF inside headers"));
        }
        if line.last() != Some(&b'\n') && n as u64 >= MAX_HEADER_LINE {
            return Err(invalid("header line too long"));
        }
        headers += 1;
        if headers > MAX_HEADERS {
            return Err(invalid("too many header lines"));
        }
        let text = String::from_utf8_lossy(&line);
        let trimmed = text.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            if headers == 1 {
                // a stray blank line between frames
                headers = 0;
                continue;
            }
            break; // end of headers
        }
        if let Some((name, value)) = trimmed.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().ok();
            }
        }
    }
    let len = content_length.ok_or_else(|| invalid("missing Content-Length"))?;
    if len > MAX_FRAME_BYTES {
        return Err(invalid(format!("frame too large: {len} > {MAX_FRAME_BYTES}")));
    }
    let mut buf = Vec::with_capacity(len.min(1 << 20));
    reader.by_ref().take(len as u64).read_to_end(&mut buf)?;
    if buf.len() < len {
        return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "EOF inside a frame body"));
    }
    let value = serde_json::from_slice(&buf).map_err(invalid_json)?;
    Ok(Some(value))
}

fn invalid_json(e: serde_json::Error) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, e)
}

/// Write a JSON-RPC response (`{"jsonrpc":"2.0","id":<id>,"result":<result>}`)
/// as a single Content-Length-framed message.
pub fn write_response<W: Write>(
    writer: &mut W,
    id: &serde_json::Value,
    result: serde_json::Value,
) -> std::io::Result<()> {
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    });
    write_frame(writer, &payload)
}

/// Write a JSON-RPC notification (no `id`) as a Content-Length-framed message.
/// Used to stream `command.output` events back to Synaps during a
/// `command.invoke` call.
pub fn write_notification<W: Write>(writer: &mut W, method: &str, params: serde_json::Value) -> std::io::Result<()> {
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
    });
    write_frame(writer, &payload)
}

fn write_frame<W: Write>(writer: &mut W, payload: &serde_json::Value) -> std::io::Result<()> {
    let body = serde_json::to_vec(payload)?;
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(&body)?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn reads_a_single_framed_message() {
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#;
        let frame = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
        let mut cursor = Cursor::new(frame.into_bytes());
        let msg = read_message(&mut cursor).unwrap().unwrap();
        assert_eq!(msg["method"], "initialize");
        assert_eq!(msg["id"], 1);
    }

    #[test]
    fn returns_none_on_clean_eof() {
        let mut cursor = Cursor::new(Vec::new());
        assert!(read_message(&mut cursor).unwrap().is_none());
    }

    #[test]
    fn errors_on_missing_content_length() {
        let mut cursor = Cursor::new(b"X-Other: 1\r\n\r\n{}".to_vec());
        assert!(read_message(&mut cursor).is_err());
    }

    #[test]
    fn a_huge_claimed_length_allocates_only_what_arrived() {
        let mut cursor = Cursor::new(b"Content-Length: 60000000\r\n\r\n{}".to_vec());
        let e = read_message(&mut cursor).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::UnexpectedEof);
        let mut cursor = Cursor::new(b"Content-Length: 99999999999\r\n\r\n{}".to_vec());
        assert_eq!(read_message(&mut cursor).unwrap_err().kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn a_bad_frame_does_not_eat_the_next_one() {
        let good = r#"{"id":2,"method":"b"}"#;
        let frame = format!("Content-Length: 3\r\n\r\n{{x}}Content-Length: {}\r\n\r\n{good}", good.len());
        let mut cursor = Cursor::new(frame.into_bytes());
        assert_eq!(read_message(&mut cursor).unwrap_err().kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(read_message(&mut cursor).unwrap().unwrap()["id"], 2);
        let mut long = vec![b'X'; 20_000];
        long.extend_from_slice(b"\r\n\r\n");
        assert!(read_message(&mut Cursor::new(long)).is_err());
    }

    #[test]
    fn round_trips_two_back_to_back_messages() {
        let a = r#"{"id":1,"method":"a"}"#;
        let b = r#"{"id":2,"method":"b"}"#;
        let frame = format!("Content-Length: {}\r\n\r\n{}Content-Length: {}\r\n\r\n{}", a.len(), a, b.len(), b);
        let mut cursor = Cursor::new(frame.into_bytes());
        let first = read_message(&mut cursor).unwrap().unwrap();
        let second = read_message(&mut cursor).unwrap().unwrap();
        assert_eq!(first["method"], "a");
        assert_eq!(second["method"], "b");
    }

    #[test]
    fn write_response_is_framed_and_readable() {
        let mut out = Vec::new();
        write_response(&mut out, &serde_json::json!(7), serde_json::json!({"action":"continue"})).unwrap();
        let mut cursor = Cursor::new(out);
        let msg = read_message(&mut cursor).unwrap().unwrap();
        assert_eq!(msg["id"], 7);
        assert_eq!(msg["result"]["action"], "continue");
    }
}
