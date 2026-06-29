//! LSP-style Content-Length-framed JSON-RPC over stdin/stdout.
//!
//! The Synaps process-extension protocol (v1): each message is
//! `Content-Length: N\r\n\r\n<N bytes of UTF-8 JSON>`. This module is the
//! transport — it knows nothing about hooks or compression.

use std::io::{BufRead, Read, Write};

/// Read one Content-Length-framed JSON message. Returns `Ok(None)` on clean
/// EOF (peer closed the pipe), `Err` on a malformed frame.
pub fn read_message<R: BufRead>(reader: &mut R) -> std::io::Result<Option<serde_json::Value>> {
    let mut content_length: Option<usize> = None;
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            return Ok(None); // EOF
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break; // end of headers
        }
        if let Some((name, value)) = trimmed.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().ok();
            }
        }
    }
    let len = content_length.ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "missing Content-Length")
    })?;
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf)?;
    let value = serde_json::from_slice(&buf)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    Ok(Some(value))
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
        let mut cursor = Cursor::new(b"\r\n{}".to_vec());
        assert!(read_message(&mut cursor).is_err());
    }

    #[test]
    fn round_trips_two_back_to_back_messages() {
        let a = r#"{"id":1,"method":"a"}"#;
        let b = r#"{"id":2,"method":"b"}"#;
        let frame = format!(
            "Content-Length: {}\r\n\r\n{}Content-Length: {}\r\n\r\n{}",
            a.len(),
            a,
            b.len(),
            b
        );
        let mut cursor = Cursor::new(frame.into_bytes());
        let first = read_message(&mut cursor).unwrap().unwrap();
        let second = read_message(&mut cursor).unwrap().unwrap();
        assert_eq!(first["method"], "a");
        assert_eq!(second["method"], "b");
    }

    #[test]
    fn write_response_is_framed_and_readable() {
        let mut out = Vec::new();
        write_response(&mut out, &serde_json::json!(7), serde_json::json!({"action":"continue"}))
            .unwrap();
        let mut cursor = Cursor::new(out);
        let msg = read_message(&mut cursor).unwrap().unwrap();
        assert_eq!(msg["id"], 7);
        assert_eq!(msg["result"]["action"], "continue");
    }
}
