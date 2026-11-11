//! W10.1 (§S, Hermes memory stack — background review fork): a fork that
//! reviews a session transcript and PROPOSES memory candidates. Two
//! hard rules ride every candidate:
//!
//! 1. **Read-before-write**: a candidate must cite its evidence — an
//!    exact excerpt that appears in the reviewed transcript. A proposal
//!    whose evidence cannot be found is refused (the fork never writes
//!    from invention).
//! 2. **Do-not-capture list**: candidates quoting secrets (API keys,
//!    bearer tokens, password assignments, private-key blocks, AWS
//!    access keys) are refused outright — the review loop must never
//!    turn a leaked credential into a persistent memory.
//!
//! Application is propose-approve: nothing writes without an explicit
//! approval (the idle_propose doctrine); the applier takes the boolean.

use serde::{Deserialize, Serialize};

/// The do-not-capture corpus (case-insensitive substring over the
/// candidate text). Patterns are conservative: a false refusal is a
/// missed memory; a false capture is a leaked secret.
const DO_NOT_CAPTURE: &[&str] = &[
    "sk-ant-",
    "sk-proj-",
    "sk-",
    "ghp_",
    "gho_",
    "github_pat_",
    "akia",
    "xoxb-",
    "xoxp-",
    "api_key=",
    "apikey=",
    "api-key:",
    "authorization: bearer",
    "set-cookie",
    "private key",
    "begin rsa",
    "begin openssh",
    "begin ec private",
    "password=",
    "passwd=",
    "secret=",
    "token=",
    "-----begin",
];

/// Maximum candidate text length (a memory is atomic — this is not a
/// transcript copy).
pub const MAX_CANDIDATE_CHARS: usize = 500;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Candidate {
    pub id: String,
    /// "memory" or "skill"
    pub kind: String,
    pub text: String,
    /// exact excerpt from the transcript proving the claim (read-before-write)
    pub evidence: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ProposalError {
    /// the candidate quotes something on the do-not-capture list
    SecretPattern(&'static str),
    /// the evidence does not appear in the reviewed transcript
    NoEvidence,
    /// empty or oversized text
    Malformed,
}

#[derive(Debug, Clone, Default)]
pub struct ReviewFork {
    /// candidates proposed so far (bounded)
    pub proposals: Vec<Candidate>,
}

impl ReviewFork {
    /// Propose a candidate from a review of `transcript`. Every rule
    /// runs BEFORE the candidate is stored.
    pub fn propose(
        &mut self,
        kind: &str,
        text: &str,
        evidence: &str,
        transcript: &[String],
    ) -> Result<Candidate, ProposalError> {
        let text = text.trim();
        let evidence = evidence.trim();
        if text.is_empty() || evidence.is_empty() {
            return Err(ProposalError::Malformed);
        }
        if text.chars().count() > MAX_CANDIDATE_CHARS {
            return Err(ProposalError::Malformed);
        }
        if !matches!(kind, "memory" | "skill") {
            return Err(ProposalError::Malformed);
        }
        // read-before-write: the evidence must appear in the transcript
        let hay = transcript.join("\n");
        if !hay.contains(evidence) {
            return Err(ProposalError::NoEvidence);
        }
        // do-not-capture: refuse secret-shaped candidates
        let lower = text.to_lowercase();
        for pat in DO_NOT_CAPTURE {
            if lower.contains(pat) {
                return Err(ProposalError::SecretPattern(pat));
            }
        }
        let c = Candidate {
            id: format!("p{}", self.proposals.len() + 1),
            kind: kind.to_string(),
            text: text.to_string(),
            evidence: evidence.to_string(),
        };
        self.proposals.push(c.clone());
        Ok(c)
    }

    /// Apply a proposal: on approval the text goes to the caller's sink
    /// (the memory store's `remember` — RELAX_WORDS promotion stays
    /// intact there); on refusal nothing is written. Returns what would
    /// be stored so the caller records the decision.
    pub fn apply(&self, id: &str, approved: bool) -> Option<(String, String)> {
        let c = self.proposals.iter().find(|c| c.id == id)?;
        if approved {
            Some((c.kind.clone(), c.text.clone()))
        } else {
            None
        }
    }
}

/// Durable-preference markers a user message must contain to be proposed
/// as a memory candidate (the heuristic surface of the review fork —
/// the fork NEVER invents a candidate; it only lifts what the user
/// actually wrote).
const PREFERENCE_MARKERS: &[&str] = &[
    "always ",
    "never ",
    "prefer ",
    "from now on",
    "remember that",
    "keep in mind",
];

/// Extract memory candidates from journal user messages: every user
/// message carrying a durable-preference marker becomes a proposal whose
/// evidence IS the message (read-before-write by construction — the
/// evidence comes verbatim from the reviewed transcript). Deterministic
/// order = journal order. Candidates are capped at
/// [`MAX_CANDIDATE_CHARS`] and filtered by the do-not-capture list via
/// [`ReviewFork::propose`], so a secret-bearing message is refused here
/// too.
pub fn fork_from_user_messages(fork: &mut ReviewFork, user_messages: &[String]) {
    for msg in user_messages {
        let lower = msg.to_lowercase();
        if let Some(marker) = PREFERENCE_MARKERS.iter().find(|m| lower.contains(*m)) {
            let text = {
                // one sentence, trimmed to the cap
                let t: String = msg.trim().chars().take(MAX_CANDIDATE_CHARS).collect();
                t
            };
            let _ = marker;
            // propose: evidence = the message itself (a verbatim excerpt)
            let _ = fork.propose("memory", &text, msg.trim(), user_messages);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transcript() -> Vec<String> {
        vec![
            "user: we deploy with scripts/deploy.sh on tuesdays".into(),
            "assistant: noted the deploy cadence".into(),
        ]
    }

    #[test]
    fn proposes_with_evidence() {
        let mut f = ReviewFork::default();
        let c = f
            .propose(
                "memory",
                "Deploy runs on Tuesdays via scripts/deploy.sh",
                "we deploy with scripts/deploy.sh on tuesdays",
                &transcript(),
            )
            .unwrap();
        assert_eq!(c.id, "p1");
        assert_eq!(f.proposals.len(), 1);
    }

    #[test]
    fn invented_evidence_refused() {
        let mut f = ReviewFork::default();
        let err = f
            .propose(
                "memory",
                "Deploy is on Mondays",
                "deploy is on mondays",
                &transcript(),
            )
            .unwrap_err();
        assert_eq!(err, ProposalError::NoEvidence);
        assert!(f.proposals.is_empty());
    }

    #[test]
    fn secret_shaped_candidates_refused() {
        let mut f = ReviewFork::default();
        let t = vec!["assistant: the key is sk-ant-api03-XXXX in .env".into()];
        let err = f
            .propose(
                "memory",
                "API key is sk-ant-api03-XXXX in .env",
                "sk-ant-api03-XXXX",
                &t,
            )
            .unwrap_err();
        assert!(matches!(err, ProposalError::SecretPattern(_)));
        // password= assignment
        let t2 = vec!["user: set PGPASSWORD=hunter2 in the env".into()];
        let err2 = f
            .propose(
                "memory",
                "PGPASSWORD=hunter2 in env",
                "PGPASSWORD=hunter2",
                &t2,
            )
            .unwrap_err();
        assert!(matches!(err2, ProposalError::SecretPattern(_)));
        assert!(f.proposals.is_empty());
    }

    #[test]
    fn malformed_inputs_refused() {
        let mut f = ReviewFork::default();
        let t = transcript();
        assert_eq!(
            f.propose("memory", "", "e", &t).unwrap_err(),
            ProposalError::Malformed
        );
        assert_eq!(
            f.propose("memory", "text", "", &t).unwrap_err(),
            ProposalError::Malformed
        );
        assert_eq!(
            f.propose("todo", "text", "text", &t).unwrap_err(),
            ProposalError::Malformed
        );
        let big = "x".repeat(MAX_CANDIDATE_CHARS + 1);
        assert_eq!(
            f.propose("memory", &big, "x", &t).unwrap_err(),
            ProposalError::Malformed
        );
    }

    #[test]
    fn apply_only_on_approval() {
        let mut f = ReviewFork::default();
        let c = f
            .propose(
                "memory",
                "deploys tuesdays",
                "deploy with scripts/deploy.sh on tuesdays",
                &transcript(),
            )
            .unwrap();
        assert!(f.apply(&c.id, false).is_none());
        assert_eq!(
            f.apply(&c.id, true),
            Some(("memory".into(), "deploys tuesdays".into()))
        );
        assert!(f.apply("p999", true).is_none());
    }

    #[test]
    fn fork_lifts_preference_messages_with_verbatim_evidence() {
        let msgs = vec![
            "please fix the tests".into(),
            "always run cargo fmt before committing".into(),
            "never push straight to main".into(),
        ];
        let mut f = ReviewFork::default();
        fork_from_user_messages(&mut f, &msgs);
        assert_eq!(f.proposals.len(), 2);
        assert_eq!(
            f.proposals[0].text,
            "always run cargo fmt before committing"
        );
        assert_eq!(
            f.proposals[0].evidence,
            "always run cargo fmt before committing"
        );
        assert_eq!(f.proposals[1].text, "never push straight to main");
    }

    #[test]
    fn fork_refuses_secret_bearing_preferences() {
        let msgs = vec!["always use API_KEY=sk-proj-abc123 for tests".into()];
        let mut f = ReviewFork::default();
        fork_from_user_messages(&mut f, &msgs);
        assert!(f.proposals.is_empty(), "do-not-capture filters the fork");
    }
}
