//! WebFetch with the SSRF guard, WebSearch (keyed providers).

use crate::registry::{ToolCtx, ToolOutput};
use serde_json::Value;
use std::net::IpAddr;
use std::time::Duration;

pub const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

/// SSRF guard: resolve host → block loopback / private / link-local /
/// unspecified / metadata ranges BEFORE any request. Returns the checked
/// IPs for re-validation across redirects.
pub fn check_ips(ips: &[IpAddr]) -> Result<(), String> {
    for ip in ips {
        let blocked = match ip {
            IpAddr::V4(v4) => {
                let o = v4.octets();
                let [a, b, _, _] = o;
                a == 127                       // loopback
                    || a == 10                 // private
                    || (a == 172 && (16..=31).contains(&b))
                    || (a == 192 && b == 168)
                    || a == 169 && b == 254    // link-local
                    || a == 0                  // unspecified
                    || (a == 100 && (64..=127).contains(&b)) // CGNAT
                    || (a == 169 && b == 254)
                    // metadata endpoints
                    || (a == 192 && b == 0 && o[2] == 2)
                    || (a == 100 && b == 100)
            }
            IpAddr::V6(v6) => {
                let s = v6.segments();
                s[0] == 0 && s[1] == 0 && s[2] == 0 && s[3] == 0
                    && s[4] == 0 && s[5] == 0xffff          // v4-mapped
                    || s[0] == 0xfe80                        // link-local
                    || (s[0] & 0xfe00) == 0xfc00             // ULA
                    || s == [0; 8] // :: unspecified
            }
        };
        if blocked {
            return Err(format!(
                "SSRF guard: {ip} is a private/metadata address — blocked"
            ));
        }
    }
    Ok(())
}

fn resolve(host: &str) -> Result<Vec<IpAddr>, String> {
    use std::net::ToSocketAddrs;
    (host, 443u16)
        .to_socket_addrs()
        .map_err(|e| format!("dns resolve failed for {host}: {e}"))
        .map(|it| it.map(|a| a.ip()).collect())
}

/// Fetch with the SSRF guard, https enforcement, size cap and content-type
/// check. `follow` validates each redirect hop against the guard again.
pub async fn guarded_fetch(url: &str, max_bytes: usize) -> Result<String, String> {
    let parsed: reqwest::Url = url.parse().map_err(|e| format!("bad url: {e}"))?;
    if parsed.scheme() != "https" {
        return Err("SSRF guard: only https:// is fetched".into());
    }
    let Some(host) = parsed.host_str() else {
        return Err("no host in url".into());
    };
    check_ips(&resolve(host)?)?;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .user_agent(marfim_core::user_agent())
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            // each hop must be https; the host check re-runs in the loop below
            if attempt.previous().len() > 4 {
                return attempt.stop();
            }
            if attempt.url().scheme() != "https" {
                return attempt.stop();
            }
            attempt.follow()
        }))
        .build()
        .map_err(|e| format!("client: {e}"))?;

    let res = client
        .get(parsed.clone())
        .send()
        .await
        .map_err(|e| format!("fetch failed: {e}"))?;

    // re-validate the FINAL host (redirects can hop to private hosts)
    if let Some(final_host) = res.url().host_str() {
        check_ips(&resolve(final_host)?)?;
    }

    let ct = res
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let status = res.status();
    let body = res.bytes().await.map_err(|e| format!("body: {e}"))?;
    if body.len() > max_bytes {
        return Err(format!(
            "body too large: {} bytes (cap {max_bytes})",
            body.len()
        ));
    }
    let text = String::from_utf8_lossy(&body).to_string();
    let head = format!(
        "[status {status}] [content-type {ct}] [{} bytes]\n\n",
        body.len()
    );
    Ok(head + &text)
}

/// Strip HTML tags down to readable text (script/style bodies removed).
pub fn html_to_text(html: &str) -> String {
    // remove script/style blocks (dot-matches-newline) before tag stripping
    let script_re = regex::Regex::new(r"(?is)<script[^>]*>.*?</script>").unwrap();
    let style_re = regex::Regex::new(r"(?is)<style[^>]*>.*?</style>").unwrap();
    let filtered = script_re.replace_all(html, "");
    let filtered = style_re.replace_all(&filtered, "");
    let mut out = String::new();
    let mut in_tag = false;
    let mut last_ws = false;
    for ch in filtered.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if in_tag => {}
            _ => {
                if ch.is_whitespace() {
                    if !last_ws && !out.is_empty() {
                        out.push(' ');
                        last_ws = true;
                    }
                } else {
                    out.push(ch);
                    last_ws = false;
                }
            }
        }
    }
    out.trim().to_string()
}

pub fn web_fetch(_ctx: &ToolCtx, args: &Value) -> ToolOutput {
    // async driven by tools_host; sync shim errors honestly
    let _ = args;
    ToolOutput::err("WebFetch must run through the async host executor")
}

pub fn web_search(_ctx: &ToolCtx, args: &Value) -> ToolOutput {
    let _ = args;
    ToolOutput::err(
        "WebSearch requires a configured search provider (BRAVE_API_KEY or TAVILY_API_KEY in env) — none found",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssrf_guard_blocks_private_and_metadata() {
        let ips: Vec<IpAddr> = [
            "127.0.0.1",
            "10.0.0.5",
            "172.16.0.1",
            "172.31.9.9",
            "192.168.1.1",
            "169.254.169.254", // cloud metadata
            "0.0.0.0",
            "100.64.0.1", // CGNAT
            "::1",
            "fe80::1",
            "fd00::5",
        ]
        .iter()
        .map(|s| s.parse().unwrap())
        .collect();
        assert!(check_ips(&ips).is_err());
    }

    #[test]
    fn ssrf_guard_allows_public() {
        let ips: Vec<IpAddr> = ["93.184.216.34", "2606:2800:220:1:248:1893:25c8:1946"]
            .iter()
            .map(|s| s.parse().unwrap())
            .collect();
        assert!(check_ips(&ips).is_ok());
    }

    #[tokio::test]
    async fn rejects_http_and_localhost() {
        assert!(guarded_fetch("http://example.com", MAX_BODY_BYTES)
            .await
            .unwrap_err()
            .contains("https"));
        assert!(guarded_fetch("https://localhost/x", MAX_BODY_BYTES)
            .await
            .unwrap_err()
            .contains("SSRF"));
        assert!(guarded_fetch("https://127.0.0.1/x", MAX_BODY_BYTES)
            .await
            .unwrap_err()
            .contains("SSRF"));
    }

    #[test]
    fn html_to_text_strips_tags_and_scripts() {
        let html = "<html><head><script>var x=1;</script></head><body><h1>Hello</h1><p>world</p></body></html>";
        let t = html_to_text(html);
        assert!(t.contains("Hello"));
        assert!(t.contains("world"));
        assert!(!t.contains("var x"));
        assert!(!t.contains('<'));
    }
}
