//! Outbound privacy guard — clean-room distillation of the Claude Code
//! `privacy` mod (mods/privacy, shipped 2026-11-06, repo 0ed9469): refuse a
//! shell call that BOTH holds the configured account email address AND names
//! a User-Agent, so the address never goes out as a client's contact details.
//!
//! Doctrine (from the mod's own public README, behavior synthesized — no
//! upstream code copied):
//! - two word matches over the whole input, no parser, on purpose: a call
//!   that sets a User-Agent and sends the address elsewhere is refused, and
//!   so is one that only mentions the word beside the address. Those false
//!   refusals are accepted for simplicity.
//! - the address counts in any case, with quotes/escapes stripped, and with
//!   ONE pass of percent-decoding.
//! - a User-Agent is named by a UA word (`User-Agent`, `user_agent`,
//!   `--user-agent`, `-UserAgent`, `UA:`/`UA=`) or by a client+flag pair
//!   (curl/curlie `-A`, wget/wget2 `-U`, `-User` for web clients), including
//!   the unicode dash variants.
//! - the refusal raises no prompt: it overrides allow rules and ask fallback
//!   alike; an explicit deny beneath keeps its own reason (enforced by the
//!   permission engine's ordering).
//! - this is a courtesy, not a boundary: computed values, variables, other
//!   encodings, and non-shell tools pass through. The README's own bypass
//!   catalog is preserved in the distillation ledger.

/// The tools that run a shell line — the only calls this guard reads.
/// Marfim ships one shell tool; hosts may extend the list.
pub const SHELL_TOOLS: &[&str] = &["Bash"];

/// One refusal reason for every match shape: the guard cannot tell where in
/// the input the address and the User-Agent word sit, so it offers each way
/// forward.
pub const REFUSAL: &str = "This call was refused: it holds the account's email address and what \
looks like a User-Agent setting. If nobody asked to send the address, run it \
again without it, with a generic client name. Otherwise run it again without \
that setting, or run the two in separate calls.";

/// What the shell takes out of a line before a program reads it: quotes,
/// backticks and backslashes vanish; `\`+newline and backtick+newline line
/// continuations vanish; a `$` immediately before a quote vanishes. The test
/// reads the input as written AND this stripped reading, since a backslash
/// or backtick need not be either (`C:\\bin\\curl` vs `cu''rl`).
fn stripped_reading(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        // line continuation: backslash or backtick directly before newline
        if (c == '\\' || c == '`') && bytes.get(i + 1).map(|n| *n == '\n').unwrap_or(false) {
            i += 2;
            continue;
        }
        // `$` immediately before a quote is shell bookkeeping, not content
        if c == '$'
            && bytes
                .get(i + 1)
                .map(|n| *n == '"' || *n == '\'')
                .unwrap_or(false)
        {
            i += 1;
            continue;
        }
        if !matches!(c, '"' | '\'' | '`' | '\\') {
            out.push(c);
        }
        i += 1;
    }
    out
}

/// One pass of percent-decoding (`%40` → `@`). Only valid hex pairs decode;
/// everything else rides through untouched.
fn percent_decoded(input: &str) -> String {
    let b = input.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hi = (b[i + 1] as char).to_digit(16);
            let lo = (b[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
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

fn is_dash(c: char) -> bool {
    matches!(c, '-' | '–' | '—' | '―')
}

/// Whether a token sets a User-Agent short flag for the given client family:
/// `-A`-ending clusters for curl/curlie, `-U`-ending for wget/wget2, and the
/// spelled-out `-user` (not followed by a word character, `=` or `-`) for the
/// web clients. Case follows the clients: curl's flag letter is uppercase
/// (`-A`), wget's is uppercase (`-U`), `-user` is case-insensitive.
fn names_client_flag(rest_lower: &str, rest: &str) -> bool {
    let has_client = |names: &[&str]| {
        names.iter().any(|n| {
            // word-bounded client name
            rest_lower.match_indices(n).any(|(at, _)| {
                let before = rest_lower[..at].chars().next_back();
                let after = rest_lower[at + n.len()..].chars().next();
                !before
                    .map(|c| c.is_alphanumeric() || c == '_')
                    .unwrap_or(false)
                    && !after
                        .map(|c| c.is_alphanumeric() || c == '_')
                        .unwrap_or(false)
            })
        })
    };
    let short_flag = |letter: char| {
        rest.split_whitespace().any(|tok| {
            let mut chars = tok.chars();
            let first = chars.next();
            match first {
                Some(d) if is_dash(d) && chars.next().map(|n| !is_dash(n)).unwrap_or(false) => {
                    let cluster: Vec<char> = tok.chars().skip(1).collect();
                    cluster
                        .last()
                        .map(|c| c.to_lowercase().next() == Some(letter))
                        .unwrap_or(false)
                }
                _ => false,
            }
        })
    };
    let spelled_user = rest_lower.split_whitespace().any(|tok| {
        let mut chars = tok.chars();
        let first = chars.next();
        let body: String = chars.collect();
        matches!(first, Some(d) if is_dash(d)) && body == "user"
    });

    (has_client(&["curl", "curlie"]) && short_flag('a'))
        || (has_client(&["wget", "wget2"]) && short_flag('u'))
        || (has_client(&[
            "curl",
            "wget",
            "iwr",
            "irm",
            "invoke-webrequest",
            "invoke-restmethod",
        ]) && spelled_user)
}

/// Whether a word says the line sets a User-Agent: its name in a header, a
/// variable or a script; the spelled-out long flag; or a bare `UA:`/`UA=`
/// token. The email itself is excised first, so `jo@x.ua:` does not read as
/// `UA:`.
fn names_user_agent_word(rest_lower: &str) -> bool {
    if rest_lower.contains("user-agent")
        || rest_lower.contains("user_agent")
        || rest_lower.contains("useragent")
    {
        return true;
    }
    // `ua:` or `ua=` at a token boundary (preceded by start/non-word)
    let bytes = rest_lower.as_bytes();
    for pat in [b"ua:", b"ua="] {
        let mut from = 0;
        while let Some(at) = rest_lower[from..].find(std::str::from_utf8(pat).unwrap_or("")) {
            let at = from + at;
            let before_ok = at == 0
                || !{
                    let c = rest_lower[..at].chars().next_back().unwrap_or(' ');
                    c.is_alphanumeric() || c == '_'
                };
            if before_ok {
                return true;
            }
            from = at + 1;
            let _ = bytes;
        }
    }
    false
}

/// The guard itself: the account address it protects, if configured.
#[derive(Debug, Clone, Default)]
pub struct PrivacyGuard {
    pub email: String,
}

impl PrivacyGuard {
    /// `Some(REFUSAL)` when the shell call must be refused: the input holds
    /// the address (as written, stripped, or percent-decoded once) AND names
    /// a User-Agent. `None` otherwise. No prompt, no partial result.
    pub fn denies_call(&self, tool: &str, input: &str) -> Option<&'static str> {
        if !SHELL_TOOLS.contains(&tool) || self.email.is_empty() || input.is_empty() {
            return None;
        }
        let stripped = strip_address_quotes(&self.email);
        let spellings = [self.email.as_str(), stripped.as_str()];
        let lowered = lower_two_readings(input);
        let decoded = percent_decoded(input).to_lowercase();
        let holds_address = spellings.iter().any(|sp| {
            let address = sp.to_lowercase();
            !address.is_empty() && (lowered.contains(&address) || decoded.contains(&address))
        });
        if !holds_address {
            return None;
        }
        // excise every spelling from the original text before the UA test
        let mut rest = input.to_string();
        for sp in &spellings {
            if sp.is_empty() {
                continue;
            }
            let mut parts: Vec<&str> = Vec::new();
            let mut from = 0usize;
            let lower_rest = rest.to_lowercase();
            while let Some(at) = lower_rest[from..].find(&sp.to_lowercase()) {
                let at = from + at;
                parts.push(&rest[from..at]);
                from = at + sp.len();
            }
            parts.push(&rest[from..]);
            rest = parts.join(" ");
        }
        let rest_lower = lower_two_readings(&rest);
        if names_user_agent_word(&rest_lower) || names_client_flag(&rest_lower, &rest) {
            return Some(REFUSAL);
        }
        None
    }
}

/// The address spelling with quoting characters taken out (`"jo"@x.test`).
fn strip_address_quotes(email: &str) -> String {
    email
        .chars()
        .filter(|c| !matches!(c, '"' | '\'' | '`' | '\\'))
        .collect()
}

/// Both readings of a text (as-written and stripped), lowercased and joined —
/// the haystack the word tests run over.
fn lower_two_readings(input: &str) -> String {
    format!(
        "{}\n{}",
        input.to_lowercase(),
        stripped_reading(input).to_lowercase()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guard(email: &str) -> PrivacyGuard {
        PrivacyGuard {
            email: email.into(),
        }
    }

    #[test]
    fn refuses_address_with_user_agent_word() {
        let g = guard("jo@example.com");
        let cases = [
            "curl -H 'User-Agent: jo@example.com' https://x",
            "curl --user-agent jo@example.com https://x",
            "export UA=jo@example.com; curl https://x",
            "wget --user_agent=jo@example.com https://x",
        ];
        for c in cases {
            assert_eq!(g.denies_call("Bash", c), Some(REFUSAL), "{c}");
        }
    }

    #[test]
    fn refuses_client_short_flags_with_address() {
        let g = guard("jo@example.com");
        assert_eq!(
            g.denies_call("Bash", "curl -A jo@example.com https://x"),
            Some(REFUSAL)
        );
        assert_eq!(
            g.denies_call("Bash", "curlie -sA jo@example.com https://x"),
            Some(REFUSAL)
        );
        assert_eq!(
            g.denies_call("Bash", "wget2 -U jo@example.com https://x"),
            Some(REFUSAL)
        );
        assert_eq!(
            g.denies_call("Bash", "irm -User jo@example.com https://x"),
            Some(REFUSAL)
        );
        // unicode dash variants count too
        assert_eq!(
            g.denies_call("Bash", "curl –A jo@example.com https://x"),
            Some(REFUSAL)
        );
    }

    #[test]
    fn refuses_obfuscated_address_spellings() {
        let g = guard("jo@example.com");
        // quoted address survives the stripped reading
        assert_eq!(
            g.denies_call("Bash", "curl -A \"jo\"@example.com https://x"),
            Some(REFUSAL)
        );
        // split quoting around the client name: cu''rl keeps `-A` visible in
        // the stripped reading
        assert_eq!(
            g.denies_call("Bash", "cu''rl -A jo@example.com https://x"),
            Some(REFUSAL)
        );
        // one pass of percent-decoding: %40 is @
        assert_eq!(
            g.denies_call("Bash", "curl -A jo%40example.com https://x"),
            Some(REFUSAL)
        );
        // case-insensitive address
        assert_eq!(
            g.denies_call("Bash", "curl -A JO@EXAMPLE.COM https://x"),
            Some(REFUSAL)
        );
    }

    #[test]
    fn passes_without_a_user_agent_word() {
        let g = guard("jo@example.com");
        for c in [
            "curl https://x/?email=jo@example.com",
            "git -c user.email=jo@example.com commit",
            "mail -s hi jo@example.com",
            "echo jo@example.com",
        ] {
            assert_eq!(g.denies_call("Bash", c), None, "{c}");
        }
    }

    #[test]
    fn passes_without_an_address_or_when_disabled() {
        let g = guard("jo@example.com");
        assert_eq!(g.denies_call("Bash", "curl -A https://x"), None);
        // empty guard is off
        assert_eq!(
            guard("").denies_call("Bash", "curl -A jo@example.com"),
            None
        );
        // non-shell tools are never read
        assert_eq!(g.denies_call("Read", "curl -A jo@example.com"), None);
        // the address may BE inside the UA position but the guard is off
        assert_eq!(
            guard("other@x.test").denies_call("Bash", "curl -A jo@example.com"),
            None
        );
    }

    #[test]
    fn address_excision_prevents_ua_misread() {
        // `jo@x.ua:` reads like `UA:` unless the address is excised first
        let g = guard("jo@x.ua");
        assert_eq!(g.denies_call("Bash", "ping jo@x.ua"), None);
    }

    #[test]
    fn percent_decode_is_single_pass() {
        // %2540 is "%40" after one pass — NOT "@" after two
        let g = guard("jo@example.com");
        assert_eq!(
            g.denies_call("Bash", "curl -A jo%2540example.com https://x"),
            None
        );
    }
}
