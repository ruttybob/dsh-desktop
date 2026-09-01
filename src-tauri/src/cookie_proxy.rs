//! Loopback cookie-injecting reverse proxy in front of the sidecar.
//!
//! WKWebView cannot be trusted with the harness's cookie dance: the session
//! cookie minted on the 303 (or injected into `WKHTTPCookieStore`) has been
//! observed never to reach subsequent requests, stranding the window on the
//! harness 401 page. Instead of fighting WebKit, the shell puts a tiny TCP
//! proxy between the WebView and the sidecar: the proxy rewrites every
//! request head to carry the minted `Cookie`, so the browser needs no cookie
//! handling at all.
//!
//! Design:
//! - one OS thread per client connection, two per proxied conversation;
//! - plain requests are rewritten with `Connection: close`, so each response
//!   terminates the conversation naturally (no keep-alive framing to track);
//! - `Upgrade: websocket` requests keep their connection semantics and are
//!   piped byte-transparently after the rewritten head (the cookie is added
//!   to the upgrade request too);
//! - request bodies travel through the raw pipe after the head is forwarded.
//!
//! The proxy binds `127.0.0.1` only and lives exactly as long as the process.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

/// Start the proxy and return the loopback port it listens on.
///
/// `cookie_pair` is the full `name=value` the harness minted for this
/// activation; it is attached to every proxied request head.
pub(crate) fn spawn(sidecar_port: u16, cookie_pair: String) -> Result<u16, String> {
    let listener =
        TcpListener::bind(("127.0.0.1", 0)).map_err(|error| format!("proxy bind failed: {error}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(client) => {
                    let cookie = cookie_pair.clone();
                    thread::spawn(move || {
                        handle(client, sidecar_port, cookie);
                    });
                }
                Err(_) => break,
            }
        }
    });
    Ok(port)
}

fn handle(mut client: TcpStream, sidecar_port: u16, cookie_pair: String) {
    let head = match read_head(&mut client) {
        Some(head) => head,
        None => {
            log::warn!("[proxy] client closed before a complete request head");
            return;
        }
    };
    let is_upgrade = is_upgrade_request(&head);
    let mut upstream = match TcpStream::connect(("127.0.0.1", sidecar_port)) {
        Ok(upstream) => upstream,
        Err(error) => {
            log::warn!("[proxy] sidecar connect failed: {error}");
            return;
        }
    };
    let rewritten = rewrite_head(&head, &cookie_pair, &format!("127.0.0.1:{sidecar_port}"), is_upgrade);
    if upstream.write_all(&rewritten).is_err() {
        return;
    }

    // Full duplex after the head: request bodies (and upgraded sockets) flow
    // client→sidecar while responses flow sidecar→client.
    let mut client_w = match client.try_clone() {
        Ok(client_w) => client_w,
        Err(_) => return,
    };
    let mut upstream_r = match upstream.try_clone() {
        Ok(upstream_r) => upstream_r,
        Err(_) => return,
    };
    let mut upstream_w = match upstream.try_clone() {
        Ok(upstream_w) => upstream_w,
        Err(_) => return,
    };
    thread::spawn(move || {
        let _ = std::io::copy(&mut client, &mut upstream_w);
        let _ = upstream_w.shutdown(std::net::Shutdown::Write);
    });
    let _ = std::io::copy(&mut upstream_r, &mut client_w);
    let _ = client_w.shutdown(std::net::Shutdown::Both);
}

/// Read one request head: everything up to and including the first `\r\n\r\n`
/// (capped). Returns `None` on EOF before a complete head or on overflow.
fn read_head(stream: &mut TcpStream) -> Option<String> {
    let mut raw = Vec::new();
    let mut buf = [0u8; 1024];
    loop {
        if raw.windows(4).any(|window| window == b"\r\n\r\n") {
            return Some(String::from_utf8_lossy(&raw).into_owned());
        }
        if raw.len() > 64 * 1024 {
            return None;
        }
        let read = stream.read(&mut buf).ok()?;
        if read == 0 {
            return None;
        }
        raw.extend_from_slice(&buf[..read]);
    }
}

fn is_upgrade_request(head: &str) -> bool {
    head.to_ascii_lowercase().contains("upgrade: websocket")
}

/// Rebuild the request head with the proxy's cookie attached and the Host
/// header pinned to the sidecar's authority. The authority names the cookie
/// server-side (`dsh-auth-<sha256(authority)>`) and is signed into its
/// payload, so without the rewrite the sidecar would look up a cookie name
/// for the proxy port and reject the request. Existing Cookie headers are
/// dropped (the proxy owns authentication); plain requests are forced to
/// `Connection: close` so the sidecar's response close terminates the
/// conversation; upgrade requests keep their connection headers apart from
/// the added Cookie.
fn rewrite_head(
    head: &str,
    cookie_pair: &str,
    sidecar_authority: &str,
    is_upgrade: bool,
) -> Vec<u8> {
    // `read_head` captures the terminating blank line too; strip it so the
    // rebuilt headers are not separated from the start line by an empty line
    // (which would end the head early and make everything after it a body —
    // the sidecar answers 400 to a headless Host).
    let head = head.trim_end_matches("\r\n").trim_end_matches('\0');
    let mut out = Vec::with_capacity(head.len() + 96);
    // The client's Host line is the proxy's authority; Origin/Referer values
    // carry it too and must be re-pointed at the sidecar (the harness rejects
    // requests whose Origin does not match its own authority).
    let proxy_authority = head
        .split("\r\n")
        .find(|line| line.to_ascii_lowercase().starts_with("host:"))
        .and_then(|line| line.split_once(':'))
        .map(|(_, value)| value.trim().to_string())
        .unwrap_or_default();
    for line in head.split("\r\n") {
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("cookie:") {
            continue;
        }
        if !is_upgrade && lower.starts_with("connection:") {
            continue;
        }
        if lower.starts_with("host:") {
            continue;
        }
        if !proxy_authority.is_empty()
            && (lower.starts_with("origin:") || lower.starts_with("referer:"))
        {
            let rewritten = line.replacen(
                proxy_authority.as_str(),
                sidecar_authority,
                1,
            );
            out.extend_from_slice(rewritten.as_bytes());
            out.extend_from_slice(b"\r\n");
            continue;
        }
        out.extend_from_slice(line.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"Host: ");
    out.extend_from_slice(sidecar_authority.as_bytes());
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(b"Cookie: ");
    out.extend_from_slice(cookie_pair.as_bytes());
    out.extend_from_slice(b"\r\n");
    if !is_upgrade {
        out.extend_from_slice(b"Connection: close\r\n");
    }
    out.extend_from_slice(b"\r\n");
    out
}

#[cfg(test)]
mod tests {
    use super::rewrite_head;

    #[test]
    fn injects_the_cookie_pins_host_and_forces_close_on_plain_requests() {
        let head = "GET / HTTP/1.1\r\nHost: 127.0.0.1:55687\r\nConnection: keep-alive\r\n\r\n";
        let out = String::from_utf8(rewrite_head(head, "dsh-auth-x=v1.a.sig", "127.0.0.1:53544", false))
            .expect("utf8");
        assert!(out.starts_with("GET / HTTP/1.1\r\n"));
        assert!(out.contains("Host: 127.0.0.1:53544\r\n"), "{out}");
        assert!(!out.contains("55687"), "the proxy port must not leak: {out}");
        assert!(!out.contains("keep-alive"));
        assert!(out.contains("Connection: close\r\n"));
        assert!(out.contains("Cookie: dsh-auth-x=v1.a.sig\r\n"));
        assert!(out.ends_with("\r\n\r\n"));
    }

    #[test]
    fn drops_client_cookies_so_the_proxy_owns_authentication() {
        let head = "GET / HTTP/1.1\r\nCookie: tracker=1\r\n\r\n";
        let out = String::from_utf8(rewrite_head(head, "dsh-auth-x=v1.a.sig", "127.0.0.1:53544", false))
            .expect("utf8");
        assert!(!out.contains("tracker=1"));
        assert!(out.contains("Cookie: dsh-auth-x=v1.a.sig\r\n"));
    }

    #[test]
    fn keeps_upgrade_connection_semantics_for_websockets() {
        let head = "GET /api/events HTTP/1.1\r\nHost: 127.0.0.1:55687\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n";
        let out = String::from_utf8(rewrite_head(head, "dsh-auth-x=v1.a.sig", "127.0.0.1:53544", true))
            .expect("utf8");
        assert!(out.contains("Connection: Upgrade\r\n"));
        assert!(!out.contains("Connection: close"));
        assert!(out.contains("Host: 127.0.0.1:53544\r\n"));
        assert!(out.contains("Cookie: dsh-auth-x=v1.a.sig\r\n"));
    }

    #[test]
    fn tolerates_a_head_without_trailing_blank_line() {
        let head = "GET / HTTP/1.1\r\nHost: h";
        let out = String::from_utf8(rewrite_head(head, "c=v", "127.0.0.1:53544", false)).expect("utf8");
        assert!(out.contains("Cookie: c=v\r\n"));
        assert!(out.contains("Host: 127.0.0.1:53544\r\n"));
        assert!(out.ends_with("\r\n\r\n"));
    }
}
