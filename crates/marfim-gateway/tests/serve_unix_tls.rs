//! W2.3: unix-socket + optional TLS serving.
//!
//! - unix socket: same protocol, same token gate, 0600 permissions,
//!   stale-socket cleanup, non-socket refusal;
//! - TLS: fail-closed boot on missing/broken PEM files; a REAL TLS 1.3
//!   handshake against a self-signed cert (openssl CLI as the client
//!   when present — honestly skipped otherwise).

use marfim_core::session_host::SessionHost;
use marfim_gateway::serve::{RouteHandlers, ServeOptions, ServeState, Server};
use std::sync::Arc;
use std::time::Duration;

fn state_with(token: &str) -> Arc<ServeState> {
    let host = Arc::new(SessionHost::new(Arc::new(move |_req| {
        Box::pin(async { Err("no factory in unit tests".to_string()) })
    })));
    ServeState::new(
        host,
        token.to_string(),
        Arc::new(vec![]),
        vec!["127.0.0.1".into(), "localhost".into()],
        ServeOptions::default(),
        RouteHandlers::default(),
        serde_json::json!({"provider": "test"}),
    )
}

#[cfg(unix)]
#[tokio::test]
async fn unix_socket_healthz_token_gate_and_permissions() {
    let dir = marfim_testkit::unique_dir("unix-serve");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("m.sock");
    let listener = Server::bind_unix(path.to_str().unwrap()).expect("bind unix");
    // permissions are 0600 right after bind
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "socket must be owner-only");
    let state = state_with("s3cret");
    tokio::spawn(async move {
        let _ = Server::new(state).serve_unix(listener).await;
    });
    // wait for the socket to accept
    let mut s = None;
    for _ in 0..50 {
        match tokio::net::UnixStream::connect(&path).await {
            Ok(c) => {
                s = Some(c);
                break;
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
        }
    }
    let mut s = s.expect("socket must become connectable");
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let req = "GET /healthz HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer wrong\r\nConnection: close\r\n\r\n";
    s.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(3), s.read_to_end(&mut buf)).await;
    let text = String::from_utf8_lossy(&buf).to_string();
    assert!(
        text.starts_with("HTTP/1.1 401"),
        "token gate holds on unix: {text}"
    );
    // right token → 200
    let mut s = tokio::net::UnixStream::connect(&path).await.unwrap();
    let req = "GET /healthz HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer s3cret\r\nConnection: close\r\n\r\n";
    s.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(3), s.read_to_end(&mut buf)).await;
    let text = String::from_utf8_lossy(&buf).to_string();
    assert!(text.starts_with("HTTP/1.1 200"), "{text}");
    assert!(text.contains("version"), "{text}");
}

#[cfg(unix)]
#[tokio::test]
async fn bind_unix_refuses_non_socket_file_and_cleans_stale_socket() {
    let dir = marfim_testkit::unique_dir("unix-bind");
    std::fs::create_dir_all(&dir).unwrap();
    // a REGULAR file at the path must never be removed
    let file_path = dir.join("regular.txt");
    std::fs::write(&file_path, "precious").unwrap();
    let err = Server::bind_unix(file_path.to_str().unwrap()).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(std::fs::read_to_string(&file_path).unwrap(), "precious");
    // a stale socket is cleaned and the bind succeeds
    let sock_path = dir.join("stale.sock");
    // create a real socket then drop it (leaves the file)
    let _ = std::os::unix::net::UnixListener::bind(&sock_path).unwrap();
    assert!(sock_path.exists());
    let listener = Server::bind_unix(sock_path.to_str().unwrap()).expect("stale socket reused");
    drop(listener);
}

#[test]
fn tls_boot_fails_closed_on_missing_or_broken_pem() {
    let dir = marfim_testkit::unique_dir("tls-boot");
    std::fs::create_dir_all(&dir).unwrap();
    let must_fail = |cert: &str, key: &str| -> String {
        match marfim_gateway::serve::tls_acceptor_from_pem_files(cert, key) {
            Err(e) => e,
            Ok(_) => panic!("TLS boot must fail for {cert}"),
        }
    };
    // missing files
    let err = must_fail(
        dir.join("nope.pem").to_str().unwrap(),
        dir.join("nope.key").to_str().unwrap(),
    );
    assert!(err.contains("tls cert"), "{err}");
    // empty cert file
    let cert = dir.join("empty.pem");
    let key = dir.join("empty.key");
    std::fs::write(&cert, "").unwrap();
    std::fs::write(&key, "").unwrap();
    let err = must_fail(cert.to_str().unwrap(), key.to_str().unwrap());
    assert!(err.contains("no certificates found"), "{err}");
}

/// A REAL TLS 1.3 handshake: self-signed cert via the openssl CLI (when
/// present), then `openssl s_client` performs the handshake and speaks
/// HTTP through it. Skipped honestly when openssl is unavailable.
#[tokio::test]
async fn tls_handshake_serves_https_end_to_end() {
    let openssl = match which_openssl() {
        Some(o) => o,
        None => {
            eprintln!("SKIP: openssl CLI unavailable; TLS handshake not exercised");
            return;
        }
    };
    let dir = marfim_testkit::unique_dir("tls-e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let cert = dir.join("cert.pem");
    let key = dir.join("key.pem");
    // -addext upgrades the self-signed cert to V3: webpki/rustls refuse V1
    // certs (UnsupportedCertVersion) and macOS ships LibreSSL, whose plain
    // `req -x509` still emits V1 — the real bug a macOS CI run caught.
    // A dialect without -addext falls back to the plain request, and if
    // the resulting cert is a V1 rustls refuses, the suite SKIPs honestly
    // (recorded, never silently).
    let req_args = |extra: &[&str]| {
        let mut a: Vec<std::ffi::OsString> = vec![
            "req".into(),
            "-x509".into(),
            "-newkey".into(),
            "rsa:2048".into(),
            "-keyout".into(),
            key.clone().into_os_string(),
            "-out".into(),
            cert.clone().into_os_string(),
            "-days".into(),
            "2".into(),
            "-nodes".into(),
            "-subj".into(),
            "/CN=localhost".into(),
        ];
        a.extend(extra.iter().map(std::ffi::OsString::from));
        a
    };
    let mut gen = std::process::Command::new(&openssl)
        .args(req_args(&["-addext", "basicConstraints=critical,CA:FALSE"]))
        .output()
        .expect("openssl req runs");
    if !gen.status.success() {
        gen = std::process::Command::new(&openssl)
            .args(req_args(&[]))
            .output()
            .expect("openssl req (fallback) runs");
        assert!(gen.status.success(), "openssl req failed: {:?}", gen.stderr);
    } else {
        assert!(gen.status.success(), "openssl req failed: {:?}", gen.stderr);
    }
    let acceptor = match marfim_gateway::serve::tls_acceptor_from_pem_files(
        cert.to_str().unwrap(),
        key.to_str().unwrap(),
    ) {
        Ok(a) => a,
        Err(e) if e.contains("UnsupportedCertVersion") => {
            eprintln!(
                "SKIP: this openssl dialect emits a V1 self-signed cert rustls refuses ({e}); TLS handshake not exercised here"
            );
            return;
        }
        Err(e) => panic!("valid self-signed PEM pair: {e}"),
    };
    let state = state_with("tls-token");
    let listener = Server::bind("127.0.0.1", 0).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let _ = Server::new(state).serve_tls(listener, acceptor).await;
    });
    // let the listener start
    tokio::time::sleep(Duration::from_millis(50)).await;
    let request = "GET /healthz HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer tls-token\r\nConnection: close\r\n\r\n";
    let connect = format!("127.0.0.1:{port}");
    let openssl_task = openssl.clone();
    // spawn_blocking + hard timeout: s_client blocks on stdin EOF, so the
    // request is written, stdin is EXPLICITLY closed, and the whole thing
    // is bounded — a wedged handshake fails the test, never hangs it.
    let out = tokio::time::timeout(
        Duration::from_secs(15),
        tokio::task::spawn_blocking(move || {
            let mut c = std::process::Command::new(&openssl_task)
                .args([
                    "s_client",
                    "-connect",
                    &connect,
                    "-servername",
                    "localhost",
                    "-quiet",
                ])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("s_client spawns");
            {
                use std::io::Write;
                c.stdin
                    .as_mut()
                    .expect("stdin piped")
                    .write_all(request.as_bytes())
                    .expect("request written");
            }
            c.stdin.take(); // EOF for s_client
            c.wait_with_output().expect("s_client finishes")
        }),
    )
    .await
    .expect("s_client must finish within 15s")
    .expect("s_client task joins");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        text.contains("HTTP/1.1 200"),
        "TLS handshake + HTTP roundtrip failed: {text}"
    );
    assert!(text.contains("version"), "{text}");
}

fn which_openssl() -> Option<String> {
    for cand in ["/usr/bin/openssl", "/bin/openssl"] {
        if std::path::Path::new(cand).exists() {
            return Some(cand.to_string());
        }
    }
    None
}

/// The TLS + unix additions must not regress the plain-TCP serve path.
#[tokio::test(flavor = "multi_thread")]
async fn plain_tcp_serve_still_roundtrips() {
    let host = Arc::new(SessionHost::new(Arc::new(move |_req| {
        Box::pin(async { Err("no factory".to_string()) })
    })));
    let state = ServeState::new(
        host,
        "t".to_string(),
        Arc::new(vec![]),
        vec!["127.0.0.1".into(), "localhost".into()],
        ServeOptions::default(),
        RouteHandlers::default(),
        serde_json::json!({"provider": "test"}),
    );
    let listener = Server::bind("127.0.0.1", 0).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let _ = Server::new(state).serve(listener).await;
    });
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    s.write_all(b"GET /healthz HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer t\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut buf = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(3), s.read_to_end(&mut buf)).await;
    let text = String::from_utf8_lossy(&buf).to_string();
    assert!(text.starts_with("HTTP/1.1 200"), "{text}");
}
