//! W9.2 — the network enforcement proxy (distillation §P, codex network
//! proxy doctrine): a LOCAL allowlist proxy a sandboxed bash run is pointed
//! at. Three protocols on one listener, all enforced against the same
//! allowlist:
//! - HTTP `CONNECT host:port` tunnels (the HTTPS path),
//! - absolute-form `GET http://host[:port]/path` (the plain-HTTP path),
//! - SOCKS5 CONNECT (greeting 05 01 00, no auth).
//!
//! Enforcement is deny-closed: a destination that is not on the allowlist
//! answers an honest refusal on its own protocol (HTTP 403 / SOCKS5 0x02)
//! and the socket closes. The proxy binds LOOPBACK only — it is an
//! enforcement point for processes we spawned, never a public relay.
//!
//! Config shape (`[sandbox]`): `networkProxy = true`,
//! `networkAllow = ["crates.io", "github.com:443", ...]` — a bare host
//! allows every port on it; `host:port` allows exactly that pair.

use std::sync::Arc;
use tokio::io::{copy_bidirectional, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// The allowlist: exact hosts (any port) or host:port pairs.
/// Deterministic evaluation, no globs — an allowlist you cannot reason
/// about is not an allowlist.
#[derive(Debug, Clone, Default)]
pub struct Allowlist {
    hosts: Vec<(String, Option<u16>)>,
}

impl Allowlist {
    pub fn from_entries(entries: &[String]) -> Result<Self, String> {
        let mut hosts = Vec::new();
        for e in entries {
            let e = e.trim();
            if e.is_empty() {
                return Err("networkAllow entries must not be empty".into());
            }
            if let Some((h, p)) = e.rsplit_once(':') {
                if h.is_empty() {
                    return Err(format!("networkAllow `{e}`: missing host"));
                }
                let port: u16 = p
                    .parse()
                    .map_err(|_| format!("networkAllow `{e}`: port must be a number"))?;
                hosts.push((h.to_ascii_lowercase(), Some(port)));
            } else {
                hosts.push((e.to_ascii_lowercase(), None));
            }
        }
        Ok(Allowlist { hosts })
    }

    /// `true` when host:port is allowed. Case-insensitive on host.
    pub fn allows(&self, host: &str, port: u16) -> bool {
        let h = host.to_ascii_lowercase();
        self.hosts
            .iter()
            .any(|(ah, ap)| *ah == h && ap.map(|p| p == port).unwrap_or(true))
    }

    pub fn is_empty(&self) -> bool {
        self.hosts.is_empty()
    }
}

/// Serve until cancelled. One task per connection (a bash run makes a
/// handful of requests — no need for an accept pool).
pub async fn serve(
    listener: TcpListener,
    allow: Arc<Allowlist>,
    mut cancel: tokio_util_like::Cancel,
) -> std::io::Result<()> {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            r = listener.accept() => {
                let (sock, _) = r?;
                let allow = allow.clone();
                tokio::spawn(async move {
                    let _ = handle(sock, allow).await;
                });
            }
        }
    }
}

// A minimal cancellation token shape (the core CancelToken is in loop_
// and carries abort semantics; the proxy needs only a watch channel).
pub mod tokio_util_like {
    use tokio::sync::watch;

    #[derive(Clone)]
    pub struct Cancel(watch::Receiver<bool>);

    #[derive(Clone)]
    pub struct CancelHandle(watch::Sender<bool>);

    pub fn cancel_channel() -> (Cancel, CancelHandle) {
        let (tx, rx) = watch::channel(false);
        (Cancel(rx), CancelHandle(tx))
    }

    impl Cancel {
        pub async fn cancelled(&mut self) {
            while !*self.0.borrow() {
                if self.0.changed().await.is_err() {
                    return;
                }
            }
        }
    }

    impl CancelHandle {
        pub fn cancel(&self) {
            let _ = self.0.send(true);
        }
    }
}

async fn handle(sock: TcpStream, allow: Arc<Allowlist>) -> std::io::Result<()> {
    // Peek enough bytes to tell the protocols apart without consuming:
    // SOCKS5 starts with 0x05, HTTP with an ASCII method.
    let mut peek = [0u8; 8];
    let n = sock.peek(&mut peek).await?;
    if n == 0 {
        return Ok(());
    }
    if peek[0] == 0x05 {
        socks5(sock, allow).await
    } else {
        http(sock, allow).await
    }
}

// ------------------------------------------------------------- HTTP paths

async fn http(mut sock: TcpStream, allow: Arc<Allowlist>) -> std::io::Result<()> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    // Read until the head terminator (the requests we care about are small).
    loop {
        let n = sock.read(&mut chunk).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 16 * 1024 {
            break;
        }
    }
    let head = String::from_utf8_lossy(&buf);
    let first_line = head.lines().next().unwrap_or("");
    let mut parts = first_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("");

    if method.eq_ignore_ascii_case("CONNECT") {
        return connect_tunnel(sock, target, allow).await;
    }
    if method.eq_ignore_ascii_case("GET") || method.eq_ignore_ascii_case("POST") {
        return absolute_form(sock, method, target, allow).await;
    }
    let _ = sock
        .write_all(b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\n\r\n")
        .await;
    Ok(())
}

async fn connect_tunnel(
    mut sock: TcpStream,
    target: &str,
    allow: Arc<Allowlist>,
) -> std::io::Result<()> {
    let Some((host, port)) = parse_host_port(target) else {
        let _ = sock
            .write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n")
            .await;
        return Ok(());
    };
    if !allow.allows(&host, port) {
        let _ = sock
            .write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
            .await;
        return Ok(());
    }
    let Ok(mut upstream) = TcpStream::connect((host.as_str(), port)).await else {
        let _ = sock
            .write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n")
            .await;
        return Ok(());
    };
    let _ = sock
        .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
        .await;
    let _ = copy_bidirectional(&mut sock, &mut upstream).await;
    Ok(())
}

async fn absolute_form(
    mut sock: TcpStream,
    method: &str,
    target: &str,
    allow: Arc<Allowlist>,
) -> std::io::Result<()> {
    // target = http://host[:port]/path
    let Some(rest) = target.strip_prefix("http://") else {
        let _ = sock
            .write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n")
            .await;
        return Ok(());
    };
    let (hostport, path) = rest.split_once('/').unwrap_or((rest, ""));
    let (host, port) = parse_host_port(hostport).unwrap_or((hostport.to_string(), 80));
    if !allow.allows(&host, port) {
        let _ = sock
            .write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
            .await;
        return Ok(());
    }
    let Ok(mut upstream) = TcpStream::connect((host.as_str(), port)).await else {
        let _ = sock
            .write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n")
            .await;
        return Ok(());
    };
    let req = format!("{method} /{path} HTTP/1.1\r\nHost: {hostport}\r\nConnection: close\r\n\r\n");
    upstream.write_all(req.as_bytes()).await?;
    let _ = copy_bidirectional(&mut sock, &mut upstream).await;
    Ok(())
}

// -------------------------------------------------------------- SOCKS5 path

async fn socks5(mut sock: TcpStream, allow: Arc<Allowlist>) -> std::io::Result<()> {
    // Greeting: VER NMETHODS METHODS — accept the no-auth offer only.
    let mut g = [0u8; 2];
    sock.read_exact(&mut g).await?;
    if g[0] != 0x05 {
        return Ok(());
    }
    let mut methods = vec![0u8; g[1] as usize];
    sock.read_exact(&mut methods).await?;
    if !methods.contains(&0x00) {
        // No no-auth offer: we cannot authenticate, refuse honestly.
        sock.write_all(&[0x05, 0xFF]).await?;
        return Ok(());
    }
    sock.write_all(&[0x05, 0x00]).await?;
    // Request: VER CMD RSV ATYP ADDR PORT
    let mut hdr = [0u8; 4];
    sock.read_exact(&mut hdr).await?;
    if hdr[1] != 0x01 {
        // Only CONNECT (0x01) is meaningful for a command runner.
        sock.write_all(&[0x05, 0x07, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
            .await?;
        return Ok(());
    }
    let (host, port) = match hdr[3] {
        0x01 => {
            let mut a = [0u8; 4];
            sock.read_exact(&mut a).await?;
            let mut p = [0u8; 2];
            sock.read_exact(&mut p).await?;
            (
                std::net::Ipv4Addr::from(a).to_string(),
                u16::from_be_bytes(p),
            )
        }
        0x03 => {
            let mut l = [0u8; 1];
            sock.read_exact(&mut l).await?;
            let mut name = vec![0u8; l[0] as usize];
            sock.read_exact(&mut name).await?;
            let mut p = [0u8; 2];
            sock.read_exact(&mut p).await?;
            (
                String::from_utf8_lossy(&name).to_string(),
                u16::from_be_bytes(p),
            )
        }
        0x04 => {
            let mut a = [0u8; 16];
            sock.read_exact(&mut a).await?;
            let mut p = [0u8; 2];
            sock.read_exact(&mut p).await?;
            (
                std::net::Ipv6Addr::from(a).to_string(),
                u16::from_be_bytes(p),
            )
        }
        _ => {
            sock.write_all(&[0x05, 0x08, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                .await?;
            return Ok(());
        }
    };
    if !allow.allows(&host, port) {
        // 0x02 = connection not allowed by ruleset.
        sock.write_all(&[0x05, 0x02, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
            .await?;
        return Ok(());
    }
    let Ok(mut upstream) = TcpStream::connect((host.as_str(), port)).await else {
        // 0x05 = connection refused by destination host.
        sock.write_all(&[0x05, 0x05, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
            .await?;
        return Ok(());
    };
    sock.write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
        .await?;
    let _ = copy_bidirectional(&mut sock, &mut upstream).await;
    Ok(())
}

fn parse_host_port(s: &str) -> Option<(String, u16)> {
    // Handles `host:port` and bracketed `[v6]:port`; a bare host is not a
    // tunnel target (CONNECT always carries a port).
    if let Some(rest) = s.strip_prefix('[') {
        let end = rest.find(']')?;
        let host = rest[..end].to_string();
        let port = rest[end + 1..].strip_prefix(':')?.parse().ok()?;
        return Some((host, port));
    }
    let (h, p) = s.rsplit_once(':')?;
    Some((h.to_string(), p.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_parses_and_evaluates() {
        let a = Allowlist::from_entries(&["crates.io".into(), "github.com:443".into()]).unwrap();
        assert!(a.allows("crates.io", 443));
        assert!(
            a.allows("CRATES.IO", 80),
            "bare host = any port, case-insensitive"
        );
        assert!(a.allows("github.com", 443));
        assert!(!a.allows("github.com", 80), "host:port is exact");
        assert!(!a.allows("evil.example", 443));
        assert!(Allowlist::from_entries(&["".into()]).is_err());
        assert!(Allowlist::from_entries(&["host:port".into()]).is_err());
        assert!(Allowlist::from_entries(&[":443".into()]).is_err());
    }

    #[tokio::test]
    async fn connect_allows_and_denies() {
        // A local echo target = the "allowed" destination.
        let echo = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let echo_port = echo.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = echo.accept().await else {
                    return;
                };
                tokio::spawn(async move {
                    let mut b = [0u8; 64];
                    if let Ok(n) = s.read(&mut b).await {
                        let _ = s.write_all(&b[..n]).await;
                    }
                    let _ = s.shutdown().await;
                });
            }
        });
        let allow = Arc::new(Allowlist::from_entries(&[format!("127.0.0.1:{echo_port}")]).unwrap());
        let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_port = proxy.local_addr().unwrap().port();
        let (cancel, handle) = tokio_util_like::cancel_channel();
        tokio::spawn(serve(proxy, allow, cancel));

        // Allowed CONNECT tunnels and pipes bytes.
        let mut c = tokio::net::TcpStream::connect(("127.0.0.1", proxy_port))
            .await
            .unwrap();
        c.write_all(
            format!("CONNECT 127.0.0.1:{echo_port} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes(),
        )
        .await
        .unwrap();
        let mut head = vec![0u8; 256];
        let n = c.read(&mut head).await.unwrap();
        assert!(String::from_utf8_lossy(&head[..n]).contains("200 Connection established"));
        c.write_all(b"ping").await.unwrap();
        let mut b = [0u8; 4];
        c.read_exact(&mut b).await.unwrap();
        assert_eq!(&b, b"ping");

        // Denied CONNECT answers 403 on the same protocol.
        let mut d = tokio::net::TcpStream::connect(("127.0.0.1", proxy_port))
            .await
            .unwrap();
        d.write_all(b"CONNECT 127.0.0.1:1 HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();
        let mut r = vec![0u8; 128];
        let n = d.read(&mut r).await.unwrap();
        assert!(String::from_utf8_lossy(&r[..n]).starts_with("HTTP/1.1 403"));
        handle.cancel();
    }

    #[tokio::test]
    async fn socks5_denies_non_allowlisted() {
        let allow = Arc::new(Allowlist::from_entries(&["crates.io:443".into()]).unwrap());
        let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_port = proxy.local_addr().unwrap().port();
        let (cancel, handle) = tokio_util_like::cancel_channel();
        tokio::spawn(serve(proxy, allow, cancel));

        let mut c = tokio::net::TcpStream::connect(("127.0.0.1", proxy_port))
            .await
            .unwrap();
        c.write_all(&[0x05, 0x01, 0x00]).await.unwrap();
        let mut g = [0u8; 2];
        c.read_exact(&mut g).await.unwrap();
        assert_eq!(g, [0x05, 0x00]);
        // CONNECT to a non-allowlisted host (domain form).
        let host = b"169.254.169.254";
        let mut req = vec![0x05, 0x01, 0x00, 0x03, host.len() as u8];
        req.extend_from_slice(host);
        req.extend_from_slice(&80u16.to_be_bytes());
        c.write_all(&req).await.unwrap();
        let mut r = [0u8; 10];
        c.read_exact(&mut r).await.unwrap();
        assert_eq!(r[1], 0x02, "denied by ruleset");
        handle.cancel();
    }

    #[tokio::test]
    async fn absolute_form_get_denies_and_forwards() {
        // A tiny local HTTP "origin".
        let origin = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin_port = origin.local_addr().unwrap().port();
        tokio::spawn(async move {
            let Ok((mut s, _)) = origin.accept().await else {
                return;
            };
            let mut b = vec![0u8; 2048];
            let _ = s.read(&mut b).await;
            let body = "hello-origin";
            let _ = s
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await;
        });
        let allow =
            Arc::new(Allowlist::from_entries(&[format!("127.0.0.1:{origin_port}")]).unwrap());
        let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_port = proxy.local_addr().unwrap().port();
        let (cancel, handle) = tokio_util_like::cancel_channel();
        tokio::spawn(serve(proxy, allow, cancel));

        // Denied first.
        let mut d = tokio::net::TcpStream::connect(("127.0.0.1", proxy_port))
            .await
            .unwrap();
        d.write_all(b"GET http://169.254.169.254/latest/meta-data HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();
        let mut r = vec![0u8; 128];
        let n = d.read(&mut r).await.unwrap();
        assert!(String::from_utf8_lossy(&r[..n]).starts_with("HTTP/1.1 403"));

        // Allowed second — the origin answers through the proxy.
        let mut c = tokio::net::TcpStream::connect(("127.0.0.1", proxy_port))
            .await
            .unwrap();
        c.write_all(
            format!("GET http://127.0.0.1:{origin_port}/x HTTP/1.1\r\nHost: o\r\n\r\n").as_bytes(),
        )
        .await
        .unwrap();
        let mut buf = vec![0u8; 256];
        let n = c.read(&mut buf).await.unwrap();
        assert!(String::from_utf8_lossy(&buf[..n]).contains("hello-origin"));
        handle.cancel();
    }
}
