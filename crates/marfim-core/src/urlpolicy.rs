//! W9.5 — the URL policy floors (distillation §G, Hermes
//! `browser_url_policy.py`): every fetch a browser/network tool performs
//! passes these floors BEFORE any connection. Deny-closed:
//!
//! 1. **IMDS is UNCONDITIONALLY denied** — cloud metadata endpoints
//!    (169.254.169.254, the `instance-data` names, IPv6 fd00:ec2::254)
//!    are never fetchable, regardless of any allow/gate configuration.
//! 2. **Private/loopback/CGNAT/localhost ranges are GATED**: allowed only
//!    when the caller explicitly opted in (`private_allowed`) — a web
//!    tool pointed at the internal network is an exfil path.
//! 3. **JS URL-literal scan**: a URL that only becomes dangerous after
//!    JS string surgery (percent-encoded dots, scheme-relative `//host`,
//!    unicode dash look-alikes in the scheme) is caught on the DECODED
//!    text, one decode pass, documented as a courtesy — not a boundary.
//!
//! Pure and deterministic; unit-tested on every floor.

/// One policy outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UrlVerdict {
    Allow,
    Deny(String),
}

/// Scan one URL literal under the policy.
pub fn check(url: &str, private_allowed: bool) -> UrlVerdict {
    // One-pass percent-decode BEFORE the tests (the JS-literal rule) —
    // %2E%2E games die here. Double-encoding is documented as passing
    // (a courtesy, not a boundary).
    let decoded = percent_decode_once(url);
    let lower = decoded.to_lowercase();

    // Unicode dash/slash look-alikes anywhere in the URL → a spoof
    // attempt (they survive visual review and re-join as separators).
    if decoded.chars().any(|c| {
        matches!(
            c,
            '\u{2010}'..='\u{2015}' | '\u{2044}' | '\u{2215}' | '\u{FF0E}'
        )
    }) {
        return UrlVerdict::Deny(
            "url contains unicode dash/slash/dot look-alike characters".to_string(),
        );
    }

    // Scheme-relative `//host` — the host check below runs on whatever
    // follows; explicit schemes we do not fetch are denied.
    let after_scheme = lower
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or_else(|| lower.trim_start_matches("//"));

    // Strip userinfo, path, port — the HOST is what the floors judge.
    // Bracketed IPv6 extracts FIRST (the colons inside are not port
    // separators).
    let hostport = after_scheme.split(['/', '?', '#']).next().unwrap_or("");
    let hostport = hostport
        .rsplit_once('@')
        .map(|(_, h)| h)
        .unwrap_or(hostport);
    let host = if let Some(rest) = hostport.strip_prefix('[') {
        match rest.find(']') {
            Some(end) => &rest[..end],
            None => hostport,
        }
    } else {
        hostport
            .rsplit_once(':')
            .map(|(h, _)| h)
            .unwrap_or(hostport)
    };
    let host = host.trim_end_matches('.');

    // Floor 1: IMDS — unconditional.
    if matches!(
        host,
        "169.254.169.254" | "fd00:ec2::254" | "instance-data" | "metadata.google.internal"
    ) {
        return UrlVerdict::Deny(format!(
            "`{host}` is a cloud metadata endpoint — denied unconditionally"
        ));
    }

    // Floor 2: private/loopback/CGNAT/link-local — gated.
    if is_private_host(host) && !private_allowed {
        return UrlVerdict::Deny(format!(
            "`{host}` is a private/loopback/link-local address — denied (private \
             destinations are gated; enable them explicitly if you mean it)"
        ));
    }
    UrlVerdict::Allow
}

fn is_private_host(host: &str) -> bool {
    // Named localhost forms.
    if host == "localhost" || host.ends_with(".localhost") || host.ends_with(".local") {
        return true;
    }
    // IPv4 literal ranges: loopback, private (RFC1918), CGNAT (65.9.0.0/16
    // is AWS; the CGNAT block is 100.64/10), link-local.
    if let Ok(ip) = host.parse::<std::net::Ipv4Addr>() {
        let o = ip.octets();
        return matches!(
            o,
            [10, ..]
                | [127, ..]
                | [172, 16..=31, ..]
                | [192, 168, ..]
                | [169, 254, ..]
                | [100, 64..=127, ..]
        );
    }
    // IPv6 literals: loopback, ULA fc00::/7, link-local fe80::/10.
    if let Ok(ip) = host.parse::<std::net::Ipv6Addr>() {
        let seg = ip.segments();
        return seg == [0, 0, 0, 0, 0, 0, 0, 1]
            || (seg[0] & 0xfe00) == 0xfc00
            || (seg[0] & 0xffc0) == 0xfe80;
    }
    false
}

/// One decode pass of %XX sequences (invalid escapes stay as written).
fn percent_decode_once(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() + 1 && i + 2 <= b.len() - 1 + 1 {
            let hex = b.get(i + 1..i + 3);
            if let Some(h) = hex {
                if let Ok(v) = u8::from_str_radix(&String::from_utf8_lossy(h), 16) {
                    out.push(v);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allow(u: &str) -> bool {
        check(u, false) == UrlVerdict::Allow
    }

    #[test]
    fn imds_is_unconditionally_denied() {
        for u in [
            "http://169.254.169.254/latest/meta-data/",
            "http://169.254.169.254:80/latest",
            "http://metadata.google.internal/computeMetadata/v1/",
            "http://instance-data/",
            "http://[fd00:ec2::254]/latest",
            // Even with private_allowed the IMDS floor stands.
        ] {
            assert!(!allow(u), "{u} must deny");
            assert!(
                matches!(check(u, true), UrlVerdict::Deny(_)),
                "{u} denies even gated"
            );
        }
    }

    #[test]
    fn private_ranges_are_gated() {
        for u in [
            "http://127.0.0.1:8080/",
            "http://10.0.0.5/x",
            "http://192.168.1.1/",
            "http://172.16.0.9/",
            "http://localhost:3000/",
            "http://100.100.1.1/", // CGNAT
            "http://[::1]/",
            "http://myhost.localhost/",
        ] {
            assert!(!allow(u), "{u} must deny ungated");
            assert!(
                matches!(check(u, true), UrlVerdict::Allow),
                "{u} allows when explicitly gated"
            );
        }
        // Public stays allowed either way.
        assert!(allow("https://crates.io/crates/x"));
        assert!(matches!(
            check("https://crates.io/x", true),
            UrlVerdict::Allow
        ));
    }

    #[test]
    fn js_url_literals_die_on_the_decoded_text() {
        // percent-encoded dots/hosts
        assert!(!allow("http://169.254.169.%32%354/latest"));
        // scheme-relative still judges the host
        assert!(!allow("//169.254.169.254/latest"));
        // trailing-dot host normalization catches the bypass
        assert!(!allow("http://169.254.169.254./latest"));
    }

    #[test]
    fn unicode_look_alike_schemes_deny() {
        // A unicode dash in the scheme position is a spoof attempt.
        assert!(matches!(
            check("http\u{2010}evil.example/x", false),
            UrlVerdict::Deny(_)
        ));
    }

    #[test]
    fn double_encoding_is_documented_as_passing() {
        // %2534 = "%34" after ONE pass — the second layer survives. The
        // honest boundary note: one decode pass is a courtesy, not a
        // boundary (the docs say so; the test pins the documented truth).
        assert!(allow("http://crates.io/%2534x"));
    }
}
