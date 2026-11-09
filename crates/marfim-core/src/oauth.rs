//! W7.4 — OAuth2 (PKCE) + credential pools (distillation §P; the
//! FORMER-REJECT adoption form for in-place key rotation: "credential
//! pool with per-request immutable snapshots — never mutate a published
//! context").
//!
//! Scope honesty:
//! - PKCE + authorize URL + token exchange/refresh are REAL (RFC 7636,
//!   including the appendix-B test vector).
//! - The login dance (browser) is NOT implemented: a headless harness
//!   cannot open a browser. The supported flow is a seeded REFRESH token
//!   (the user performs the browser dance once with any tool they trust,
//!   then pastes the refresh token into `[credentials.oauth.*]`); Marfim
//!   refreshes access tokens from there, machine-side, forever.
//! - Pool rotation IS the failover walk: a pooled credential expands to
//!   one immutable snapshot per chain entry, so a 401 on one snapshot
//!   fails over to the next — no published context is ever mutated.

use crate::config::Config;
use crate::credentials::CredentialSource;
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------- PKCE

/// RFC 7636 §4.1: the verifier is 43..128 chars from the unreserved set.
const VERIFIER_CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";

/// Generate a PKCE code verifier. Randomness: `getrandom`'s OS source
/// (a verifier is a per-request secret, NOT a determinism surface —
/// the seeded-RNG doctrine governs output-affecting paths only, and this
/// value is never journaled, never logged, never rendered).
pub fn pkce_verifier(len: usize) -> Result<String, String> {
    if !(43..=128).contains(&len) {
        return Err("pkce verifier length must be 43..=128 (RFC 7636 §4.1)".into());
    }
    let mut bytes = vec![0u8; len];
    getrandom::fill(&mut bytes).map_err(|e| format!("os randomness unavailable: {e}"))?;
    let out: String = bytes
        .iter()
        .map(|b| VERIFIER_CHARS[(*b as usize) % VERIFIER_CHARS.len()] as char)
        .collect();
    Ok(out)
}

/// RFC 7636 §4.2: `BASE64URL-ENCODE(SHA256(ASCII(code_verifier)))` with
/// the padding stripped.
pub fn pkce_challenge_s256(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

/// The full authorize URL for the browser dance the USER performs
/// (Marfim never opens one). Pure string assembly, testable.
pub fn authorize_url(
    endpoint: &str,
    client_id: &str,
    redirect_uri: &str,
    state: &str,
    challenge: &str,
    scopes: &[&str],
) -> String {
    let scope = scopes.join("%20");
    let mut url = format!(
        "{endpoint}?response_type=code&client_id={client_id}&redirect_uri={redirect_uri}\
         &state={state}&code_challenge={challenge}&code_challenge_method=S256"
    );
    if !scope.is_empty() {
        url.push_str("&scope=");
        url.push_str(&scope);
    }
    url
}

/// The token-endpoint form body for the authorization-code exchange.
pub fn exchange_body(code: &str, redirect_uri: &str, client_id: &str, verifier: &str) -> String {
    format!(
        "grant_type=authorization_code&code={code}&redirect_uri={redirect_uri}\
         &client_id={client_id}&code_verifier={verifier}"
    )
}

/// The token-endpoint form body for a refresh grant.
pub fn refresh_body(refresh_token: &str, client_id: &str) -> String {
    format!("grant_type=refresh_token&refresh_token={refresh_token}&client_id={client_id}")
}

// ---------------------------------------------------------- token store

/// One OAuth token set. `expires_at_epoch` is unix seconds; `None` means
/// the endpoint never said (treated as never-expiring — honest, the wire
/// will say otherwise if wrong).
#[derive(Debug, Clone, PartialEq)]
pub struct OAuthToken {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at_epoch: Option<u64>,
}

/// The 60s refresh skew: a token within a minute of expiry is refreshed
/// before it is used, so a slow request never ships a dead token.
pub const EXPIRY_SKEW_SECS: u64 = 60;

impl OAuthToken {
    pub fn is_expired(&self, now_epoch: u64) -> bool {
        match self.expires_at_epoch {
            Some(at) => now_epoch.saturating_add(EXPIRY_SKEW_SECS) >= at,
            None => false,
        }
    }
}

/// Parse a token-endpoint JSON response (documented OAuth2 shape:
/// `access_token`, optional `refresh_token`, optional `expires_in`).
pub fn parse_token_response(body: &str) -> Result<OAuthToken, String> {
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("token response not JSON: {e}"))?;
    let access = v["access_token"]
        .as_str()
        .ok_or_else(|| "token response has no access_token".to_string())?;
    let expires_in = v["expires_in"].as_u64();
    Ok(OAuthToken {
        access_token: access.to_string(),
        refresh_token: v["refresh_token"].as_str().map(String::from),
        expires_at_epoch: expires_in.map(|s| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
                .saturating_add(s)
        }),
    })
}

/// POST the refresh grant to the token endpoint (REAL HTTP, one try —
/// the caller owns retries) and parse the response.
pub async fn refresh_access_token(
    client: &reqwest::Client,
    token_url: &str,
    client_id: &str,
    refresh_token: &str,
) -> Result<OAuthToken, String> {
    let res = client
        .post(token_url)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("accept", "application/json")
        .body(refresh_body(refresh_token, client_id))
        .send()
        .await
        .map_err(|e| format!("token endpoint unreachable: {e}"))?;
    let status = res.status();
    let text = res
        .text()
        .await
        .map_err(|e| format!("token endpoint read failed: {e}"))?;
    if !status.is_success() {
        return Err(format!(
            "token endpoint HTTP {status}: {}",
            crate::providers::openai::truncate(&text, 300)
        ));
    }
    let mut tok = parse_token_response(&text)?;
    // RFC 6749 §6: a refresh grant MAY rotate the refresh token; when the
    // endpoint stays silent, the old one remains valid — carry it.
    if tok.refresh_token.is_none() {
        tok.refresh_token = Some(refresh_token.to_string());
    }
    Ok(tok)
}

// ------------------------------------------------- OAuth credential source

/// The `[credentials.oauth.<name>]` entry.
#[derive(Debug, Clone, PartialEq)]
pub struct OAuthConfig {
    pub token_url: String,
    pub client_id: String,
    pub refresh_token: String,
    pub access_token: Option<String>,
    pub expires_in_secs: Option<u64>,
}

impl OAuthConfig {
    pub fn from_value(v: &serde_json::Value) -> Result<Self, String> {
        let get_s = |k: &str| -> Result<String, String> {
            v[k].as_str()
                .map(String::from)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| format!("credentials.oauth entry missing `{k}`"))
        };
        Ok(OAuthConfig {
            token_url: get_s("tokenUrl")?,
            client_id: get_s("clientId")?,
            refresh_token: get_s("refreshToken")?,
            access_token: v["accessToken"].as_str().map(String::from),
            expires_in_secs: v["expiresInSecs"].as_u64(),
        })
    }
}

/// A [`CredentialSource`] that answers with the CURRENT access token for
/// its name, refreshing machine-side when the stored token is (nearly)
/// expired. Resolution is sync (the W1.3 trait); the refresh happens in
/// `refresh_if_needed` — the factory calls it once at session build (an
/// async context), and `resolve` answers from the store afterwards. A
/// token that is expired AND has never been refreshed resolves to the
/// seed access token (if any) or NOTHING — never a fabricated value.
pub struct OAuthSource {
    name: String,
    cfg: OAuthConfig,
    client: reqwest::Client,
    current: Mutex<OAuthToken>,
}

impl OAuthSource {
    pub fn new(name: &str, cfg: OAuthConfig) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let current = OAuthToken {
            access_token: cfg.access_token.clone().unwrap_or_default(),
            refresh_token: Some(cfg.refresh_token.clone()),
            expires_at_epoch: cfg
                .access_token
                .as_ref()
                .map(|_| now.saturating_add(cfg.expires_in_secs.unwrap_or(0))),
        };
        Self {
            name: name.to_string(),
            cfg,
            client: crate::http_client(),
            current: Mutex::new(current),
        }
    }

    /// The name this source answers for (the preset's `api_key_env`).
    pub fn credential_name(&self) -> &str {
        &self.name
    }

    /// Refresh now if the stored token is (nearly) expired. One HTTP try;
    /// the error is returned for the caller to surface, and the previous
    /// token stays in place (a failed refresh never blanks a working
    /// credential).
    pub async fn refresh_if_needed(&self) -> Result<bool, String> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let expired = {
            self.current
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .is_expired(now)
        };
        if !expired {
            return Ok(false);
        }
        let rt = self
            .current
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .refresh_token
            .clone()
            .unwrap_or_default();
        let fresh =
            refresh_access_token(&self.client, &self.cfg.token_url, &self.cfg.client_id, &rt)
                .await?;
        *self.current.lock().unwrap_or_else(|p| p.into_inner()) = fresh;
        Ok(true)
    }

    /// Test/admin view: the stored token's expiry.
    pub fn expires_at(&self) -> Option<u64> {
        self.current
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .expires_at_epoch
    }
}

impl CredentialSource for OAuthSource {
    fn resolve(&self, name: &str) -> Option<String> {
        if name != self.name {
            return None;
        }
        let t = self.current.lock().unwrap_or_else(|p| p.into_inner());
        Some(t.access_token.clone()).filter(|s| !s.is_empty())
    }

    fn name(&self) -> &'static str {
        "oauth"
    }
}

// ------------------------------------------------------- credential pool

/// A named pool of credentials (the FORMER-REJECT adoption form):
/// `snapshot()` hands out an IMMUTABLE per-entry value — rotation is the
/// chain expansion (one snapshot per chain entry) plus the existing
/// failover walk, never an in-place mutation of a published context.
#[derive(Default)]
pub struct CredentialPool {
    entries: Mutex<BTreeMaplike>,
}

// BTreeMap with Vec<String> per name; a type alias keeps the struct honest.
type BTreeMaplike = std::collections::BTreeMap<String, Vec<String>>;

impl CredentialPool {
    pub fn new() -> Self {
        Self::default()
    }

    /// Read `[credentialPools.<NAME>] values = [...]` — every value is a
    /// snapshot; an entry with fewer than 2 values is NOT a pool (the
    /// plain config/env chain covers it) and is refused honestly.
    pub fn from_config(config: &Config) -> Result<Self, String> {
        let mut entries = BTreeMaplike::new();
        if let Some(v) = config.get("credentialPools") {
            let obj = v
                .as_object()
                .ok_or_else(|| "credentialPools section must be an object".to_string())?;
            for (name, entry) in obj {
                let vals: Vec<String> = entry["values"]
                    .as_array()
                    .ok_or_else(|| format!("credentialPools.{name}.values must be an array"))?
                    .iter()
                    .map(|x| {
                        x.as_str().map(String::from).ok_or_else(|| {
                            format!("credentialPools.{name}.values must hold strings")
                        })
                    })
                    .collect::<Result<_, _>>()?;
                if vals.len() < 2 {
                    return Err(format!(
                        "credentialPools.{name}: a pool needs >= 2 values (use [credentials] for one)"
                    ));
                }
                if vals.iter().any(String::is_empty) {
                    return Err(format!("credentialPools.{name}: empty values are refused"));
                }
                entries.insert(name.clone(), vals);
            }
        }
        Ok(CredentialPool {
            entries: Mutex::new(entries),
        })
    }

    /// All snapshot values for `name`, in config order (deterministic).
    pub fn snapshots(&self, name: &str) -> Vec<Arc<str>> {
        self.entries
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(name)
            .map(|v| v.iter().map(|s| Arc::from(s.as_str())).collect())
            .unwrap_or_default()
    }

    pub fn names(&self) -> Vec<String> {
        self.entries
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .keys()
            .cloned()
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.entries
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // RFC 7636 Appendix B — the official test vector.
    const RFC_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    const RFC_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

    #[test]
    fn pkce_s256_matches_the_rfc_vector() {
        assert_eq!(pkce_challenge_s256(RFC_VERIFIER), RFC_CHALLENGE);
    }

    #[test]
    fn verifier_length_and_alphabet_hold() {
        let v = pkce_verifier(64).unwrap();
        assert_eq!(v.len(), 64);
        assert!(v.chars().all(|c| VERIFIER_CHARS.contains(&(c as u8))));
        assert!(pkce_verifier(42).is_err(), "below the RFC floor");
        assert!(pkce_verifier(129).is_err(), "above the RFC ceiling");
    }

    #[test]
    fn authorize_url_carries_the_pkce_params() {
        let url = authorize_url(
            "https://auth.example/authorize",
            "cid",
            "http://127.0.0.1:9/cb",
            "st-123",
            RFC_CHALLENGE,
            &["read", "write"],
        );
        assert!(url.starts_with("https://auth.example/authorize?"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains(&format!("code_challenge={RFC_CHALLENGE}")));
        assert!(url.contains("scope=read%20"));
    }

    #[test]
    fn exchange_and_refresh_bodies_are_urlencoded_form_shape() {
        let b = exchange_body("the-code", "http://127.0.0.1:9/cb", "cid", RFC_VERIFIER);
        assert!(b.starts_with("grant_type=authorization_code&code=the-code"));
        assert!(b.contains(&format!("code_verifier={RFC_VERIFIER}")));
        let r = refresh_body("rt-1", "cid");
        assert_eq!(
            r,
            "grant_type=refresh_token&refresh_token=rt-1&client_id=cid"
        );
    }

    #[test]
    fn token_expiry_respects_the_skew() {
        let t = OAuthToken {
            access_token: "at".into(),
            refresh_token: None,
            expires_at_epoch: Some(10_000),
        };
        assert!(!t.is_expired(10_000 - 61), "61s out: still fresh");
        assert!(t.is_expired(10_000 - 60), "exactly at the skew: refresh");
        assert!(t.is_expired(10_001));
        let open = OAuthToken {
            expires_at_epoch: None,
            ..t.clone()
        };
        assert!(!open.is_expired(0), "no expiry info = never claims expired");
    }

    #[test]
    fn token_response_parses_and_requires_access_token() {
        let t = parse_token_response(
            r#"{"access_token":"at-1","refresh_token":"rt-2","expires_in":3600}"#,
        )
        .unwrap();
        assert_eq!(t.access_token, "at-1");
        assert_eq!(t.refresh_token.as_deref(), Some("rt-2"));
        assert!(t.expires_at_epoch.is_some());
        assert!(parse_token_response(r#"{"error":"nope"}"#).is_err());
        assert!(parse_token_response("not json").is_err());
    }

    #[tokio::test]
    async fn refresh_grant_posts_the_form_and_keeps_the_old_refresh_token() {
        // A REAL local token endpoint: asserts the wire shape and the
        // RFC 6749 §6 carry-forward.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen: Arc<Mutex<String>> = Default::default();
        let seen2 = seen.clone();
        tokio::spawn(async move {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut buf = vec![0u8; 65536];
            let n = sock.read(&mut buf).await.unwrap_or(0);
            *seen2.lock().unwrap() = String::from_utf8_lossy(&buf[..n]).to_string();
            let body = r#"{"access_token":"fresh-at","expires_in":600}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
        });
        let tok = refresh_access_token(
            &crate::http_client(),
            &format!("http://127.0.0.1:{port}/token"),
            "cid",
            "seed-rt",
        )
        .await
        .unwrap();
        assert_eq!(tok.access_token, "fresh-at");
        // The endpoint sent no refresh token → the seed carries forward.
        assert_eq!(tok.refresh_token.as_deref(), Some("seed-rt"));
        let req = seen.lock().unwrap().clone();
        assert!(req.contains("grant_type=refresh_token&refresh_token=seed-rt&client_id=cid"));
        assert!(req.contains("POST /token"));
    }

    #[test]
    fn oauth_source_resolves_only_its_own_name_and_never_fabricates() {
        let src = OAuthSource::new(
            "ANTHROPIC_OAUTH",
            OAuthConfig {
                token_url: "http://127.0.0.1:1/token".into(),
                client_id: "cid".into(),
                refresh_token: "rt".into(),
                access_token: Some("seed-at".into()),
                expires_in_secs: Some(3600),
            },
        );
        assert_eq!(src.resolve("ANTHROPIC_OAUTH").as_deref(), Some("seed-at"));
        assert_eq!(src.resolve("OTHER"), None, "fail-closed by lookup");
        let bare = OAuthSource::new(
            "BARE",
            OAuthConfig {
                token_url: "http://127.0.0.1:1/token".into(),
                client_id: "cid".into(),
                refresh_token: "rt".into(),
                access_token: None,
                expires_in_secs: None,
            },
        );
        assert_eq!(bare.resolve("BARE"), None, "no seed + no refresh = nothing");
    }

    #[test]
    fn pool_refuses_non_pools_and_hands_out_snapshots_in_order() {
        let cfg = Config::from_json_str(
            r#"{"credentialPools": {"K": {"values": ["one", "two", "three"]}}}"#,
        )
        .unwrap();
        let pool = CredentialPool::from_config(&cfg).unwrap();
        assert_eq!(pool.names(), vec!["K".to_string()]);
        let snaps = pool.snapshots("K");
        assert_eq!(snaps.len(), 3);
        assert_eq!(&*snaps[0], "one");
        assert_eq!(&*snaps[2], "three");
        assert!(pool.snapshots("absent").is_empty());

        // A single value is not a pool — honest refusal.
        let cfg =
            Config::from_json_str(r#"{"credentialPools": {"K": {"values": ["one"]}}}"#).unwrap();
        assert!(CredentialPool::from_config(&cfg).is_err());
        // Empty values refused.
        let cfg =
            Config::from_json_str(r#"{"credentialPools": {"K": {"values": ["", "x"]}}}"#).unwrap();
        assert!(CredentialPool::from_config(&cfg).is_err());
    }
}
