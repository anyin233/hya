//! Loopback HTTP callback server for authorization-code OAuth (RFC 8252).

use std::collections::HashMap;
use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::time::{Duration, Instant};

use super::OAuthError;

/// Wait for a single OAuth redirect on a loopback port.
///
/// Returns query parameters from the callback URL (`code`, `state`, `error`, …).
pub fn wait_for_callback(
    host: &str,
    port: u16,
    path: &str,
    timeout: Duration,
) -> Result<HashMap<String, String>, OAuthError> {
    let addr: SocketAddr = format!("{host}:{port}")
        .parse()
        .map_err(|e| OAuthError::Protocol(format!("invalid callback address: {e}")))?;
    let listener = TcpListener::bind(addr).map_err(|e| {
        OAuthError::Protocol(format!(
            "could not bind OAuth callback on http://{host}:{port}{path}: {e}"
        ))
    })?;
    listener.set_nonblocking(true).map_err(OAuthError::Io)?;
    let deadline = Instant::now() + timeout;
    loop {
        if Instant::now() >= deadline {
            return Err(OAuthError::Timeout(format!(
                "timed out waiting for OAuth callback on http://{host}:{port}{path}"
            )));
        }
        match listener.accept() {
            Ok((stream, _)) => {
                if let Some(params) = handle_connection(stream, path)? {
                    return Ok(params);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(OAuthError::Io(e)),
        }
    }
}

fn handle_connection(
    mut stream: TcpStream,
    expected_path: &str,
) -> Result<Option<HashMap<String, String>>, OAuthError> {
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let mut buf = [0u8; 8192];
    let n = stream.read(&mut buf).unwrap_or(0);
    let request = String::from_utf8_lossy(&buf[..n]);
    let request_line = request.lines().next().unwrap_or("");
    let target = request_line.split_whitespace().nth(1).unwrap_or("/");
    let (req_path, query) = match target.split_once('?') {
        Some((p, q)) => (p, q),
        None => (target, ""),
    };
    if req_path != expected_path {
        let _ = write_response(&mut stream, 404, "Not Found", "Unexpected callback path.");
        return Ok(None);
    }
    let params = parse_query(query);
    let error = params.get("error").cloned();
    let body = if let Some(err) = error.as_deref() {
        let desc = params
            .get("error_description")
            .map(String::as_str)
            .unwrap_or("authorization failed");
        format!("Sign-in failed: {err} — {desc}. You can close this tab.")
    } else {
        "Sign-in complete. You can close this browser tab and return to the terminal.".to_string()
    };
    let _ = write_response(&mut stream, 200, "OK", &body);
    Ok(Some(params))
}

fn parse_query(query: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        out.insert(url_decode(k), url_decode(v));
    }
    out
}

fn url_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = &s[i + 1..i + 3];
                if let Ok(v) = u8::from_str_radix(hex, 16) {
                    out.push(v as char);
                    i += 3;
                } else {
                    out.push('%');
                    i += 1;
                }
            }
            c => {
                out.push(c as char);
                i += 1;
            }
        }
    }
    out
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    reason: &str,
    message: &str,
) -> std::io::Result<()> {
    let html = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{reason}</title></head>\
         <body style=\"font-family:system-ui;padding:2rem\"><h1>{reason}</h1><p>{message}</p></body></html>"
    );
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{html}",
        html.len()
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpStream;
    use std::thread;

    #[test]
    fn captures_code_and_state_from_callback() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let handle = thread::spawn(move || {
            wait_for_callback("127.0.0.1", port, "/auth/callback", Duration::from_secs(5)).unwrap()
        });
        thread::sleep(Duration::from_millis(50));
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let req = "GET /auth/callback?code=abc%20123&state=xyz HTTP/1.1\r\nHost: localhost\r\n\r\n";
        stream.write_all(req.as_bytes()).unwrap();
        let params = handle.join().unwrap();
        assert_eq!(params.get("code").map(String::as_str), Some("abc 123"));
        assert_eq!(params.get("state").map(String::as_str), Some("xyz"));
    }
}
