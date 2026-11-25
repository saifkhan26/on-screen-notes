//! Just enough HTTP/1.1 for the browser link: one request per
//! connection, its head read whole, a body of a stated length, and a
//! short JSON answer. Nothing here knows what the requests mean.

use std::io::{Read, Write};

/// A request head is never longer than this; one that is, is turned away.
const MAX_HEAD: usize = 32 << 10;

/// A request's first line and headers, and what was read past them.
#[derive(Debug)]
pub struct Request {
    pub method: String,
    /// The path without its query.
    pub path: String,
    headers: Vec<(String, String)>,
    /// Body bytes that came in with the head.
    pub body_start: Vec<u8>,
}

impl Request {
    /// The value of header `name`, any case.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    /// The header's value, percent-decoded (the extension encodes every
    /// value it sends, so addresses and names stay plain ASCII).
    pub fn text(&self, name: &str) -> String {
        self.header(name).map(pct_decode).unwrap_or_default()
    }

    pub fn content_length(&self) -> Option<u64> {
        self.header("content-length")?.trim().parse().ok()
    }
}

#[derive(Debug, PartialEq)]
pub enum HeadError {
    /// The connection closed or timed out before the head was in.
    Cut,
    TooLong,
    Malformed,
}

/// Read a request head from `stream`.
pub fn read_head(stream: &mut impl Read) -> Result<Request, HeadError> {
    let mut buf: Vec<u8> = Vec::with_capacity(2048);
    let mut chunk = [0u8; 4096];
    loop {
        let n = stream.read(&mut chunk).map_err(|_| HeadError::Cut)?;
        if n == 0 {
            return Err(HeadError::Cut);
        }
        // Look for the blank line only around what just came in.
        let from = buf.len().saturating_sub(3);
        buf.extend_from_slice(&chunk[..n]);
        if let Some(end) = buf[from..].windows(4).position(|w| w == b"\r\n\r\n").map(|p| from + p) {
            if end > MAX_HEAD {
                return Err(HeadError::TooLong);
            }
            let mut req = parse_head(&buf[..end])?;
            req.body_start = buf[end + 4..].to_vec();
            return Ok(req);
        }
        if buf.len() > MAX_HEAD {
            return Err(HeadError::TooLong);
        }
    }
}

/// The request line and headers, from the bytes before the blank line.
pub fn parse_head(head: &[u8]) -> Result<Request, HeadError> {
    let text = std::str::from_utf8(head).map_err(|_| HeadError::Malformed)?;
    let mut lines = text.split("\r\n");
    let first = lines.next().ok_or(HeadError::Malformed)?;
    let mut parts = first.split(' ');
    let (Some(method), Some(target), Some(version)) = (parts.next(), parts.next(), parts.next()) else {
        return Err(HeadError::Malformed);
    };
    if !version.starts_with("HTTP/1.") || method.is_empty() || !target.starts_with('/') {
        return Err(HeadError::Malformed);
    }
    let path = target.split('?').next().unwrap_or(target).to_string();
    let mut headers = Vec::new();
    for line in lines {
        let (k, v) = line.split_once(':').ok_or(HeadError::Malformed)?;
        headers.push((k.trim().to_string(), v.trim().to_string()));
    }
    Ok(Request { method: method.to_string(), path, headers, body_start: Vec::new() })
}

/// Write an answer of type `ctype` and let the connection close.
pub fn respond_typed(stream: &mut impl Write, status: u16, ctype: &str, body: &[u8]) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        408 => "Request Timeout",
        411 => "Length Required",
        413 => "Payload Too Large",
        415 => "Unsupported Media Type",
        503 => "Service Unavailable",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

/// `{"error":"<what>"}`.
pub fn error_json(what: &str) -> String {
    format!("{{\"error\":{}}}", json_str(what))
}

/// `s` as a JSON string, quoted.
pub fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `%XX` sequences turned back into bytes, read as UTF-8 (`+` stays `+`:
/// the extension encodes with `encodeURIComponent`).
pub fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = |c: u8| (c as char).to_digit(16);
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Whether a request's `Host` is this machine on `port`. A web address
/// that a page points at 127.0.0.1 ("DNS rebinding") brings its own name
/// here, and is turned away.
pub fn host_ok(host: Option<&str>, port: u16) -> bool {
    let Some(host) = host else { return false };
    let host = host.trim().to_ascii_lowercase();
    [format!("127.0.0.1:{port}"), format!("localhost:{port}")].contains(&host)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_head_is_read_with_the_body_bytes_after_it() {
        let raw = b"POST /v1/media?x=1 HTTP/1.1\r\nHost: 127.0.0.1:47823\r\nX-OSN: 1\r\nContent-Length: 5\r\n\r\nhel";
        let req = read_head(&mut &raw[..]).unwrap();
        assert_eq!(req.method, "POST");
        assert_eq!(req.path, "/v1/media");
        assert_eq!(req.header("x-osn"), Some("1"));
        assert_eq!(req.header("HOST"), Some("127.0.0.1:47823"));
        assert_eq!(req.content_length(), Some(5));
        assert_eq!(req.body_start, b"hel");
    }

    #[test]
    fn cut_short_or_odd_heads_are_turned_away() {
        assert_eq!(read_head(&mut &b"GET / HTTP/1.1\r\nHost: x"[..]).unwrap_err(), HeadError::Cut);
        assert_eq!(read_head(&mut &b"GET\r\n\r\n"[..]).unwrap_err(), HeadError::Malformed);
        assert_eq!(read_head(&mut &b"GET / HTTP/1.1\r\nno colon\r\n\r\n"[..]).unwrap_err(), HeadError::Malformed);
        let long = format!("GET / HTTP/1.1\r\nX: {}\r\n\r\n", "a".repeat(MAX_HEAD + 10));
        assert_eq!(read_head(&mut long.as_bytes()).unwrap_err(), HeadError::TooLong);
    }

    #[test]
    fn percent_decoding_reads_utf8_and_leaves_stray_percents() {
        assert_eq!(pct_decode("https%3A%2F%2Fx.com%2Fa_b%2Fstatus%2F1"), "https://x.com/a_b/status/1");
        assert_eq!(pct_decode("%E3%81%82rt"), "あrt");
        assert_eq!(pct_decode("100%"), "100%");
        assert_eq!(pct_decode("%zz%4"), "%zz%4");
        assert_eq!(pct_decode("a+b"), "a+b");
    }

    #[test]
    fn json_strings_are_escaped() {
        assert_eq!(json_str("a\"b\\c\n"), "\"a\\\"b\\\\c\\n\"");
        assert_eq!(error_json("pair"), "{\"error\":\"pair\"}");
    }

    #[test]
    fn only_this_machine_on_this_port_is_a_good_host() {
        assert!(host_ok(Some("127.0.0.1:47823"), 47823));
        assert!(host_ok(Some("LOCALHOST:47823"), 47823));
        assert!(!host_ok(Some("127.0.0.1:80"), 47823));
        assert!(!host_ok(Some("evil.example:47823"), 47823));
        assert!(!host_ok(None, 47823));
    }

    #[test]
    fn answers_say_how_long_they_are() {
        let mut out = Vec::new();
        respond_typed(&mut out, 200, "application/json", b"{\"ok\":true}").unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(text.contains("Content-Length: 11\r\n"));
        assert!(text.ends_with("\r\n\r\n{\"ok\":true}"));
    }
}
