//! Port of oracle `src/intel/confusables.js` — the confusable-pair
//! clarifier (Group A4).
//!
//! Some settings tools SWAP (arena failure classes); for these a confident
//! route is a coin flip. Two honest halves:
//!
//! 1. MINE (offline, deterministic, committed artifact): candidate pairs are
//!    same-group same-kind tools whose tool documents are near-duplicates
//!    under BM25+ (floor 0.55), plus 7 hand-pinned pairs from the arena's
//!    failure classes. The highest-gain question ships as the pair's ask.
//! 2. CLARIFY (runtime, read-only): when the router verdict is AMBIGUOUS
//!    and the ranked candidates contain a mined pair, the generic question
//!    is REPLACED by the pair's question. No verdict changes, no write.
//!
//! PORT NOTES / NARROWINGS:
//! - The committed artifact (`src/intel/data/kde/confusable_pairs.json`)
//!   is embedded verbatim as [`COMMITTED_PAIRS_JSON`] — the runtime never
//!   mines on the fly and never reads files, exactly like the oracle's
//!   load-once contract (a corrupt artifact would degrade to `[]`).
//! - The oracle's `docSimilarity`/`minePairs` take a vectorize `TfidfIndex`;
//!   the tokenizer lives in `vectorize.js` (another module's port). This
//!   module therefore owns ONLY the BM25+ scoring over PRE-TOKENIZED
//!   documents ([`DocIndex`]); fixtures record `index.docTokens` verbatim.
//!   The oracle's fallback `tokenize(aKey)` for keys missing from the index
//!   is unrepresentable here: a missing key scores 0.0 (and docSimilarity
//!   returns 0.0 because the self-score is 0).
//! - The oracle's `minePairs(docs, ...)` first parameter is dead code in the
//!   oracle source (verified) and is not carried over.
//! - `clarify`'s `runner` injection maps to `Option<&[ConfusablePair]>`:
//!   `None` reads the committed artifact (the oracle's default `{}`).
//! - Row sorting in the oracle uses `localeCompare` only via the dead
//!   parameter path; the live sorts are JS `<` (UTF-16 code-unit order ==
//!   codepoint order for the ASCII tool names used here).

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::LazyLock;

use crate::intel::dp::decompose_f64;

/// BM25 near-duplicate threshold over tool documents (mined candidates
/// must clear this before a reviewer question is even drafted).
pub const DOC_SIM_FLOOR: f64 = 0.55;

const GENERIC_SCALE_QUESTION: &str =
    "an ABSOLUTE value in that unit, or a SCALE multiplier on the current value?";

/// A committed or mined confusable pair (identical shape on both sides of
/// the oracle: loadPairs() rows and minePairs() rows).
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ConfusablePair {
    pub a: String,
    pub b: String,
    #[serde(default)]
    pub group: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub doc_similarity: f64,
    #[serde(default)]
    pub question: String,
    #[serde(default)]
    pub answers: BTreeMap<String, String>,
    #[serde(default)]
    pub gain: f64,
    #[serde(default)]
    pub evidence: String,
    #[serde(default)]
    pub source: String,
}

/// The committed artifact, embedded verbatim from the oracle's
/// `src/intel/data/kde/confusable_pairs.json` (runtime never reads files).
pub const COMMITTED_PAIRS_JSON: &str = r###"{
 "generator": "assistant.core.confusables.mine_pairs",
 "doc_sim_floor": 0.55,
 "n_pairs": 27,
 "pairs": [
  {
   "a": "setAudioIncrement",
   "b": "setBrightnessIncrement",
   "group": "services",
   "kind": "float",
   "doc_similarity": 0.7523,
   "question": "the step size for VOLUME, or for SCREEN BRIGHTNESS?",
   "answers": {
    "volume": "setAudioIncrement",
    "brightness": "setBrightnessIncrement"
   },
   "gain": 1.0,
   "evidence": "dev m25/m26: identical sentence one noun apart; the brightness sibling wins both at cold start",
   "source": "hand-pinned (arena failure class)"
  },
  {
   "a": "setEnableOverviewBlur",
   "b": "setDisableWallpaperBlur",
   "group": "overview",
   "kind": "bool",
   "doc_similarity": 0.9322,
   "question": "the blur of the OVERVIEW ITSELF (windows floating in it), or the WALLPAPER's blur while the overview is open?",
   "answers": {
    "overview itself": "setEnableOverviewBlur",
    "wallpaper": "setDisableWallpaperBlur"
   },
   "gain": 1.0,
   "evidence": "dev m11/m12 (sealed sr67/sr68 same class): the two overview blur toggles swap under on/off phrasing",
   "source": "hand-pinned (arena failure class)"
  },
  {
   "a": "setMaxFprintTries",
   "b": "setPreviewScalesWirelessPassword",
   "group": "lockscreen",
   "kind": "int",
   "doc_similarity": 0.0,
   "question": "how many FINGERPRINT attempts before the password fallback — or how the password PREVIEW is scaled?",
   "answers": {
    "fingerprint attempts": "setMaxFprintTries",
    "preview": "setPreviewScalesWirelessPassword"
   },
   "gain": 1.0,
   "evidence": "dev m14: 'fingerprint attempts before the password' lands on a preview-scale tool via the 'password' noun",
   "source": "hand-pinned (arena failure class)"
  },
  {
   "a": "setPosition",
   "b": "setHoverBottomRight",
   "group": "notifications",
   "kind": "enum",
   "doc_similarity": 0.0,
   "question": "where NOTIFICATION toasts appear on screen, or the overview corner that shows windows on HOVER?",
   "answers": {
    "toast": "setPosition",
    "hover": "setHoverBottomRight"
   },
   "gain": 1.0,
   "evidence": "dev m08: 'show toasts in the bottom-right corner' confidently takes the overview hover-corner toggle",
   "source": "hand-pinned (arena failure class)"
  },
  {
   "a": "setTrayRecolour",
   "b": "setDockRecolourIcons",
   "group": "bar",
   "kind": "bool",
   "doc_similarity": 0.6804,
   "question": "which icons — the system TRAY icons, or the DOCK app icons?",
   "answers": {
    "tray": "setTrayRecolour",
    "dock": "setDockRecolourIcons"
   },
   "gain": 1.0,
   "evidence": "dev m02: 'recolour the tray icons to match my theme' ranks the dock sibling first (sealed sr13 same class)",
   "source": "hand-pinned (arena failure class)"
  },
  {
   "a": "setBarScale",
   "b": "setPreviewScale",
   "group": "bar",
   "kind": "float",
   "doc_similarity": 0.9156,
   "question": "an ABSOLUTE value in that unit, or a SCALE multiplier on the current value?",
   "answers": {
    "scale multiplier": "setBarScale",
    "absolute value": "setPreviewScale"
   },
   "gain": 1.0,
   "evidence": "mined: same-group 'bar' documents similarity 0.9156",
   "source": "mined (document twins)"
  },
  {
   "a": "setBarShowOnHover",
   "b": "setGreeterShowOnHover",
   "group": "bar",
   "kind": "bool",
   "doc_similarity": 0.953,
   "question": "the bar setting, or the greeter setting?",
   "answers": {
    "bar": "setBarShowOnHover",
    "greeter": "setGreeterShowOnHover"
   },
   "gain": 1.0,
   "evidence": "mined: same-group 'bar' documents similarity 0.953",
   "source": "mined (document twins)"
  },
  {
   "a": "setDesktopClockInvertColors",
   "b": "setDesktopLyricsInvertColors",
   "group": "wallpaper-scheme",
   "kind": "bool",
   "doc_similarity": 0.9071,
   "question": "the clock setting, or the lyrics setting?",
   "answers": {
    "clock": "setDesktopClockInvertColors",
    "lyrics": "setDesktopLyricsInvertColors"
   },
   "gain": 1.0,
   "evidence": "mined: same-group 'wallpaper-scheme' documents similarity 0.9071",
   "source": "mined (document twins)"
  },
  {
   "a": "setDesktopIconsEnabled",
   "b": "setMaterialYouIconsEnabled",
   "group": "wallpaper-scheme",
   "kind": "bool",
   "doc_similarity": 0.8865,
   "question": "the desktop setting, or the material setting?",
   "answers": {
    "desktop": "setDesktopIconsEnabled",
    "material": "setMaterialYouIconsEnabled"
   },
   "gain": 1.0,
   "evidence": "mined: same-group 'wallpaper-scheme' documents similarity 0.8865",
   "source": "mined (document twins)"
  },
  {
   "a": "setPreviewScale",
   "b": "setPreviewScalesAudio",
   "group": "bar",
   "kind": "float",
   "doc_similarity": 0.9536,
   "question": "an ABSOLUTE value in that unit, or a SCALE multiplier on the current value?",
   "answers": {
    "scale multiplier": "setPreviewScale",
    "absolute value": "setPreviewScalesAudio"
   },
   "gain": 1.0,
   "evidence": "mined: same-group 'bar' documents similarity 0.9536",
   "source": "mined (document twins)"
  },
  {
   "a": "setPreviewScale",
   "b": "setPreviewScalesBattery",
   "group": "bar",
   "kind": "float",
   "doc_similarity": 0.9536,
   "question": "an ABSOLUTE value in that unit, or a SCALE multiplier on the current value?",
   "answers": {
    "scale multiplier": "setPreviewScale",
    "absolute value": "setPreviewScalesBattery"
   },
   "gain": 1.0,
   "evidence": "mined: same-group 'bar' documents similarity 0.9536",
   "source": "mined (document twins)"
  },
  {
   "a": "setPreviewScale",
   "b": "setPreviewScalesBluetooth",
   "group": "bar",
   "kind": "float",
   "doc_similarity": 0.9536,
   "question": "an ABSOLUTE value in that unit, or a SCALE multiplier on the current value?",
   "answers": {
    "scale multiplier": "setPreviewScale",
    "absolute value": "setPreviewScalesBluetooth"
   },
   "gain": 1.0,
   "evidence": "mined: same-group 'bar' documents similarity 0.9536",
   "source": "mined (document twins)"
  },
  {
   "a": "setPreviewScale",
   "b": "setPreviewScalesDock",
   "group": "bar",
   "kind": "float",
   "doc_similarity": 0.9536,
   "question": "an ABSOLUTE value in that unit, or a SCALE multiplier on the current value?",
   "answers": {
    "scale multiplier": "setPreviewScale",
    "absolute value": "setPreviewScalesDock"
   },
   "gain": 1.0,
   "evidence": "mined: same-group 'bar' documents similarity 0.9536",
   "source": "mined (document twins)"
  },
  {
   "a": "setPreviewScale",
   "b": "setPreviewScalesGithub",
   "group": "bar",
   "kind": "float",
   "doc_similarity": 0.9536,
   "question": "an ABSOLUTE value in that unit, or a SCALE multiplier on the current value?",
   "answers": {
    "scale multiplier": "setPreviewScale",
    "absolute value": "setPreviewScalesGithub"
   },
   "gain": 1.0,
   "evidence": "mined: same-group 'bar' documents similarity 0.9536",
   "source": "mined (document twins)"
  },
  {
   "a": "setPreviewScale",
   "b": "setPreviewScalesGreeter",
   "group": "bar",
   "kind": "float",
   "doc_similarity": 0.9536,
   "question": "an ABSOLUTE value in that unit, or a SCALE multiplier on the current value?",
   "answers": {
    "scale multiplier": "setPreviewScale",
    "absolute value": "setPreviewScalesGreeter"
   },
   "gain": 1.0,
   "evidence": "mined: same-group 'bar' documents similarity 0.9536",
   "source": "mined (document twins)"
  },
  {
   "a": "setPreviewScale",
   "b": "setPreviewScalesLockStatus",
   "group": "bar",
   "kind": "float",
   "doc_similarity": 0.9135,
   "question": "an ABSOLUTE value in that unit, or a SCALE multiplier on the current value?",
   "answers": {
    "scale multiplier": "setPreviewScale",
    "absolute value": "setPreviewScalesLockStatus"
   },
   "gain": 1.0,
   "evidence": "mined: same-group 'bar' documents similarity 0.9135",
   "source": "mined (document twins)"
  },
  {
   "a": "setPreviewScale",
   "b": "setPreviewScalesNetwork",
   "group": "bar",
   "kind": "float",
   "doc_similarity": 0.9536,
   "question": "an ABSOLUTE value in that unit, or a SCALE multiplier on the current value?",
   "answers": {
    "scale multiplier": "setPreviewScale",
    "absolute value": "setPreviewScalesNetwork"
   },
   "gain": 1.0,
   "evidence": "mined: same-group 'bar' documents similarity 0.9536",
   "source": "mined (document twins)"
  },
  {
   "a": "setPreviewScale",
   "b": "setPreviewScalesNotifications",
   "group": "bar",
   "kind": "float",
   "doc_similarity": 0.9536,
   "question": "an ABSOLUTE value in that unit, or a SCALE multiplier on the current value?",
   "answers": {
    "scale multiplier": "setPreviewScale",
    "absolute value": "setPreviewScalesNotifications"
   },
   "gain": 1.0,
   "evidence": "mined: same-group 'bar' documents similarity 0.9536",
   "source": "mined (document twins)"
  },
  {
   "a": "setPreviewScale",
   "b": "setPreviewScalesPeripheralBattery",
   "group": "bar",
   "kind": "float",
   "doc_similarity": 0.9135,
   "question": "an ABSOLUTE value in that unit, or a SCALE multiplier on the current value?",
   "answers": {
    "scale multiplier": "setPreviewScale",
    "absolute value": "setPreviewScalesPeripheralBattery"
   },
   "gain": 1.0,
   "evidence": "mined: same-group 'bar' documents similarity 0.9135",
   "source": "mined (document twins)"
  },
  {
   "a": "setPreviewScale",
   "b": "setPreviewScalesTrayMenu",
   "group": "bar",
   "kind": "float",
   "doc_similarity": 0.8784,
   "question": "an ABSOLUTE value in that unit, or a SCALE multiplier on the current value?",
   "answers": {
    "scale multiplier": "setPreviewScale",
    "absolute value": "setPreviewScalesTrayMenu"
   },
   "gain": 1.0,
   "evidence": "mined: same-group 'bar' documents similarity 0.8784",
   "source": "mined (document twins)"
  },
  {
   "a": "setPreviewScale",
   "b": "setPreviewScalesWirelessPassword",
   "group": "bar",
   "kind": "float",
   "doc_similarity": 0.9135,
   "question": "an ABSOLUTE value in that unit, or a SCALE multiplier on the current value?",
   "answers": {
    "scale multiplier": "setPreviewScale",
    "absolute value": "setPreviewScalesWirelessPassword"
   },
   "gain": 1.0,
   "evidence": "mined: same-group 'bar' documents similarity 0.9135",
   "source": "mined (document twins)"
  },
  {
   "a": "setPreviewScalesBattery",
   "b": "setPreviewScalesPeripheralBattery",
   "group": "bar",
   "kind": "float",
   "doc_similarity": 0.9555,
   "question": "an ABSOLUTE value in that unit, or a SCALE multiplier on the current value?",
   "answers": {
    "scale multiplier": "setPreviewScalesBattery",
    "absolute value": "setPreviewScalesPeripheralBattery"
   },
   "gain": 1.0,
   "evidence": "mined: same-group 'bar' documents similarity 0.9555",
   "source": "mined (document twins)"
  },
  {
   "a": "setShowDashboard",
   "b": "setShowMedia",
   "group": "dashboard",
   "kind": "bool",
   "doc_similarity": 0.9325,
   "question": "the dashboard setting, or the media setting?",
   "answers": {
    "dashboard": "setShowDashboard",
    "media": "setShowMedia"
   },
   "gain": 1.0,
   "evidence": "mined: same-group 'dashboard' documents similarity 0.9325",
   "source": "mined (document twins)"
  },
  {
   "a": "setShowDashboard",
   "b": "setShowPerformance",
   "group": "dashboard",
   "kind": "bool",
   "doc_similarity": 0.9325,
   "question": "the dashboard setting, or the performance setting?",
   "answers": {
    "dashboard": "setShowDashboard",
    "performance": "setShowPerformance"
   },
   "gain": 1.0,
   "evidence": "mined: same-group 'dashboard' documents similarity 0.9325",
   "source": "mined (document twins)"
  },
  {
   "a": "setShowDashboard",
   "b": "setShowTerminal",
   "group": "dashboard",
   "kind": "bool",
   "doc_similarity": 0.9325,
   "question": "the dashboard setting, or the terminal setting?",
   "answers": {
    "dashboard": "setShowDashboard",
    "terminal": "setShowTerminal"
   },
   "gain": 1.0,
   "evidence": "mined: same-group 'dashboard' documents similarity 0.9325",
   "source": "mined (document twins)"
  },
  {
   "a": "setShowDashboard",
   "b": "setShowWeather",
   "group": "dashboard",
   "kind": "bool",
   "doc_similarity": 0.9325,
   "question": "the dashboard setting, or the weather setting?",
   "answers": {
    "dashboard": "setShowDashboard",
    "weather": "setShowWeather"
   },
   "gain": 1.0,
   "evidence": "mined: same-group 'dashboard' documents similarity 0.9325",
   "source": "mined (document twins)"
  },
  {
   "a": "setShowMedia",
   "b": "setUseMediaShapes",
   "group": "dashboard",
   "kind": "bool",
   "doc_similarity": 0.9323,
   "question": "the show setting, or the use setting?",
   "answers": {
    "show": "setShowMedia",
    "use": "setUseMediaShapes"
   },
   "gain": 1.0,
   "evidence": "mined: same-group 'dashboard' documents similarity 0.9323",
   "source": "mined (document twins)"
  }
 ]
}"###;

/// The committed CONFUSABLE_PAIRS (runtime view of the artifact, loaded
/// once; a missing/corrupt artifact degrades to [] — the clarifier falls
/// back to the generic question, never mines live, never crashes).
pub static CONFUSABLE_PAIRS: LazyLock<Vec<ConfusablePair>> = LazyLock::new(|| {
    #[derive(serde::Deserialize)]
    struct Artifact {
        #[serde(default)]
        pairs: Vec<ConfusablePair>,
    }
    serde_json::from_str::<Artifact>(COMMITTED_PAIRS_JSON)
        .map(|a| a.pairs)
        .unwrap_or_default()
});

/// The committed pairs list ([] when the artifact is absent/corrupt).
pub fn load_pairs() -> &'static [ConfusablePair] {
    &CONFUSABLE_PAIRS
}

/// Hand-pinned pair from the arena's failure classes (m-items): the sibling
/// swaps the router measurably confuses. Each cites the dev item that
/// evidences the confusion. A hand pin overrides the generated question when
/// both exist (reviewer judgment beats a template).
#[derive(Debug, Clone)]
pub struct HandPin {
    pub a: &'static str,
    pub b: &'static str,
    pub evidence: &'static str,
    pub question: &'static str,
    pub answers: &'static [(&'static str, &'static str)],
    pub gain: f64,
}

/// Hand-pinned pairs from the arena's own failure classes (m-items).
pub const HAND_PINNED: [HandPin; 7] = [
    HandPin {
        a: "setTrayRecolour",
        b: "setDockRecolourIcons",
        evidence: "dev m02: 'recolour the tray icons to match my theme' ranks the dock sibling first (sealed sr13 same class)",
        question: "which icons — the system TRAY icons, or the DOCK app icons?",
        answers: &[("tray", "setTrayRecolour"), ("dock", "setDockRecolourIcons")],
        gain: 1.0,
    },
    HandPin {
        a: "setDesktopLyricsPosition",
        b: "setDesktopLyricsAlignment",
        evidence: "dev m15/m16: 'move the lyrics to the centre' and 'centre the lyrics text' rank each other's sibling first (sealed sr93 same class)",
        question: "where the lyrics SIT on screen, or how the text ALIGNS inside its box?",
        answers: &[
            ("where", "setDesktopLyricsPosition"),
            ("align", "setDesktopLyricsAlignment"),
        ],
        gain: 1.0,
    },
    HandPin {
        a: "setAudioIncrement",
        b: "setBrightnessIncrement",
        evidence: "dev m25/m26: identical sentence one noun apart; the brightness sibling wins both at cold start",
        question: "the step size for VOLUME, or for SCREEN BRIGHTNESS?",
        answers: &[
            ("volume", "setAudioIncrement"),
            ("brightness", "setBrightnessIncrement"),
        ],
        gain: 1.0,
    },
    HandPin {
        a: "setPosition",
        b: "setHoverBottomRight",
        evidence: "dev m08: 'show toasts in the bottom-right corner' confidently takes the overview hover-corner toggle",
        question: "where NOTIFICATION toasts appear on screen, or the overview corner that shows windows on HOVER?",
        answers: &[("toast", "setPosition"), ("hover", "setHoverBottomRight")],
        gain: 1.0,
    },
    HandPin {
        a: "setAmbientOpacity",
        b: "setTransparencyBase",
        evidence: "dev m05: 'make the ambient background more opaque' takes the global transparency slider",
        question: "the AMBIENT background layer's opacity, or the GLOBAL transparency of the whole shell?",
        answers: &[
            ("ambient", "setAmbientOpacity"),
            ("global", "setTransparencyBase"),
        ],
        gain: 1.0,
    },
    HandPin {
        a: "setEnableOverviewBlur",
        b: "setDisableWallpaperBlur",
        evidence: "dev m11/m12 (sealed sr67/sr68 same class): the two overview blur toggles swap under on/off phrasing",
        question: "the blur of the OVERVIEW ITSELF (windows floating in it), or the WALLPAPER's blur while the overview is open?",
        answers: &[
            ("overview itself", "setEnableOverviewBlur"),
            ("wallpaper", "setDisableWallpaperBlur"),
        ],
        gain: 1.0,
    },
    HandPin {
        a: "setMaxFprintTries",
        b: "setPreviewScalesWirelessPassword",
        evidence: "dev m14: 'fingerprint attempts before the password' lands on a preview-scale tool via the 'password' noun",
        question: "how many FINGERPRINT attempts before the password fallback — or how the password PREVIEW is scaled?",
        answers: &[
            ("fingerprint attempts", "setMaxFprintTries"),
            ("preview", "setPreviewScalesWirelessPassword"),
        ],
        gain: 1.0,
    },
];

// ---------------------------------------------------------------------------
// Pre-tokenized BM25+ index (the scoring half of vectorize.TfidfIndex).
// ---------------------------------------------------------------------------

/// Okapi BM25+ constants of the oracle's TfidfIndex default config.
const K1: f64 = 1.5;
const B: f64 = 0.75;
const DELTA: f64 = 1.0;

/// The BM25+ document index over PRE-TOKENIZED documents — exactly the
/// observable surface of the oracle's `TfidfIndex.docTokens` +
/// `index.score(tokens, key)` used by docSimilarity/minePairs.
#[derive(Debug, Clone, Default)]
pub struct DocIndex {
    pub doc_tokens: BTreeMap<String, Vec<String>>,
    df: BTreeMap<String, usize>,
    avg_len: f64,
    n_docs: usize,
}

impl DocIndex {
    /// Build from (key, tokens) pairs — key order does not affect any
    /// observable (df/avgLen are order-insensitive sums of integers).
    pub fn new(pairs: Vec<(String, Vec<String>)>) -> Self {
        let mut doc_tokens: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (k, toks) in pairs {
            doc_tokens.insert(k, toks);
        }
        let mut df: BTreeMap<String, usize> = BTreeMap::new();
        let mut total_len = 0.0f64;
        for toks in doc_tokens.values() {
            total_len += toks.len().max(1) as f64;
            let mut seen: BTreeSet<&str> = BTreeSet::new();
            for t in toks {
                if seen.insert(t.as_str()) {
                    *df.entry(t.clone()).or_insert(0) += 1;
                }
            }
        }
        let n_docs = doc_tokens.len();
        let avg_len = total_len / n_docs.max(1) as f64;
        Self {
            doc_tokens,
            df,
            avg_len,
            n_docs,
        }
    }

    /// BM25+ score of `query_tokens` against the document at `key`
    /// (oracle vectorize.js `TfidfIndex._scoreOne`, k1=1.5 b=0.75 delta=1.0).
    pub fn score(&self, query_tokens: &[String], key: &str) -> f64 {
        let Some(toks) = self.doc_tokens.get(key) else {
            return 0.0;
        };
        if toks.is_empty() {
            return 0.0;
        }
        let mut tf: BTreeMap<&str, i64> = BTreeMap::new();
        for t in toks {
            *tf.entry(t.as_str()).or_insert(0) += 1;
        }
        let dl = toks.len().max(1) as f64;
        let norm = K1 * (1.0 - B + B * dl / self.avg_len);
        let mut total = 0.0f64;
        for term in query_tokens {
            let f = tf.get(term.as_str()).copied().unwrap_or(0);
            if f == 0 {
                continue;
            }
            let df = self.df.get(term.as_str()).copied().unwrap_or(0) as f64;
            let idf = ((self.n_docs as f64 - df + 0.5) / (df + 0.5) + 1.0).ln();
            total += (idf * (f as f64 * (K1 + 1.0))) / (f as f64 + norm) + DELTA;
        }
        total
    }
}

/// BM25 score of a's document tokens against b's document (a KEY into the
/// index), normalized by the self-max (0..1-ish; the pair floor is
/// deliberately low — mining proposes, the reviewer disposes).
pub fn doc_similarity(index: &DocIndex, a_key: &str, b_key: &str) -> f64 {
    // NARROWING vs the oracle: a key missing from the index would fall back
    // to vectorize.tokenize(aKey) there; tokenization is not owned here, so
    // a missing key yields empty tokens -> self-score 0 -> 0.0.
    let Some(toks) = index.doc_tokens.get(a_key) else {
        return 0.0;
    };
    let toks = toks.clone();
    let score = index.score(&toks, b_key);
    let self_score = index.score(&toks, a_key);
    if self_score <= 0.0 {
        return 0.0;
    }
    score / self_score
}

/// JS `Number(x.toFixed(4))`: the integer n closest to x*10^4 over the
/// EXACT binary value (ties pick the LARGER n, i.e. half-up for positive
/// values), then n/10^4. Exact via integer arithmetic.
pub(crate) fn js_to_fixed4(x: f64) -> f64 {
    if !x.is_finite() || x == 0.0 {
        return x;
    }
    let neg = x < 0.0;
    let (mant, e) = decompose_f64(x.abs());
    let m = mant as u128 * 10_000u128; // < 2^53 * 2^14 = 2^67
    let q: u128 = if e >= 0 {
        let shift = e as u32;
        if shift >= 61 || (m >> (127 - shift)) != 0 {
            return x; // |x| >= 2^43: 4-decimal rounding cannot change it
        }
        m << shift
    } else {
        let k = (-e) as u32;
        if k >= 128 {
            0 // den > 2r for every r < 2^67: rounds to 0
        } else {
            let den = 1u128 << k;
            let q0 = m / den;
            let r = m % den;
            let twice = r * 2; // r < den <= 2^127 -> 2r < 2^128
                               // JS toFixed ties pick the larger n; over a negative x the two
                               // candidates are negative, so "larger" means toward zero.
            if twice == den {
                if neg {
                    q0
                } else {
                    q0 + 1
                }
            } else if twice > den {
                q0 + 1
            } else {
                q0
            }
        }
    };
    let v = (q as f64) / 10_000.0;
    if neg {
        -v
    } else {
        v
    }
}

/// lexicon.js `camelSplit` — `/[A-Z]?[a-z]+|[A-Z]+(?![a-z])/g` matches,
/// lowercased. Hand-rolled with the regex engine's per-position
/// alternation order and `[A-Z]+` backtracking reproduced exactly.
pub(crate) fn camel_split(name: &str) -> Vec<String> {
    let chars: Vec<char> = name.chars().collect();
    let is_upper = |c: char| c.is_ascii_uppercase();
    let is_lower = |c: char| c.is_ascii_lowercase();
    let mut out: Vec<String> = Vec::new();
    let mut pos = 0usize;
    while pos < chars.len() {
        let c = chars[pos];
        // alternative 1: [A-Z]?[a-z]+
        let mut j = pos;
        if is_upper(c) {
            j += 1;
        }
        if j < chars.len() && is_lower(chars[j]) {
            while j < chars.len() && is_lower(chars[j]) {
                j += 1;
            }
            let word: String = chars[pos..j].iter().collect();
            out.push(word.to_lowercase());
            pos = j;
            continue;
        }
        // alternative 2: [A-Z]+(?![a-z]) — greedy run with backtracking
        if is_upper(c) {
            let mut k = pos;
            while k < chars.len() && is_upper(chars[k]) {
                k += 1;
            }
            while k > pos + 1 && k < chars.len() && is_lower(chars[k]) {
                k -= 1;
            }
            if !(k < chars.len() && is_lower(chars[k])) {
                let word: String = chars[pos..k].iter().collect();
                out.push(word.to_lowercase());
                pos = k;
                continue;
            }
        }
        // no match at this position: the /g engine advances one character
        pos += 1;
    }
    out
}

/// ToolSpec-like object supplying name/group/kind (+ optional numeric range
/// for the scale-vs-absolute question). None = absent/null in the oracle.
#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ToolSpec {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum: Option<f64>,
}

/// A drafted distinguishing question ({question, answers, gain}).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct QuestionDraft {
    pub question: String,
    pub answers: BTreeMap<String, String>,
    pub gain: f64,
}

/// Draft the distinguishing question from the metadata difference. Highest
/// expected information gain wins; with two hypotheses the prior entropy is
/// 1 bit, so gain = P(answer separates) x 1 bit. Returns None when the pair
/// is dropped honestly.
pub fn question_for(a: &ToolSpec, b: &ToolSpec) -> Option<QuestionDraft> {
    for (s, other) in [(a, b), (b, a)] {
        let lower = s.name.to_lowercase();
        if lower.contains("scale") || lower.contains("strength") {
            if let (Some(min), Some(max)) = (s.minimum, s.maximum) {
                if max - min <= 4.0 {
                    return Some(QuestionDraft {
                        question: GENERIC_SCALE_QUESTION.to_string(),
                        answers: BTreeMap::from([
                            ("scale multiplier".to_string(), s.name.clone()),
                            ("absolute value".to_string(), other.name.clone()),
                        ]),
                        gain: 1.0,
                    });
                }
            }
        }
    }
    let skip = ["set", "enable", "disabled", "enabled"];
    let wa: Vec<String> = camel_split(&a.name)
        .into_iter()
        .filter(|w| !skip.contains(&w.as_str()))
        .collect();
    let wb: Vec<String> = camel_split(&b.name)
        .into_iter()
        .filter(|w| !skip.contains(&w.as_str()))
        .collect();
    let diff_a = wa.iter().find(|w| !wb.contains(*w));
    let diff_b = wb.iter().find(|w| !wa.contains(*w));
    if let (Some(da), Some(db)) = (diff_a, diff_b) {
        return Some(QuestionDraft {
            question: format!("the {da} setting, or the {db} setting?"),
            answers: BTreeMap::from([(da.clone(), a.name.clone()), (db.clone(), b.name.clone())]),
            gain: 1.0,
        });
    }
    None
}

/// Deterministic pair mining: hand-pinned classes + same-group same-kind
/// document twins above the similarity floor. Returns
/// {generator, doc_sim_floor, n_pairs, pairs}.
pub fn mine_pairs(index: &DocIndex, specs: &[ToolSpec]) -> MinePairsResult {
    let spec_by_name: BTreeMap<&str, &ToolSpec> =
        specs.iter().map(|s| (s.name.as_str(), s)).collect();

    // group -> [spec index], insertion-ordered like the oracle's Map
    let mut group_order: Vec<String> = Vec::new();
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, s) in specs.iter().enumerate() {
        let g = s.group.clone().unwrap_or_default();
        if !groups.contains_key(&g) {
            group_order.push(g.clone());
        }
        groups.entry(g).or_default().push(i);
    }
    let mut sorted_groups = group_order.clone();
    sorted_groups.sort();

    // generated candidates: same group, same kind, twin documents
    let mut generated: Vec<(String, String, String, f64)> = Vec::new(); // (a, b, group, sim)
    let mut seen: HashSet<String> = HashSet::new();
    for group in &sorted_groups {
        let idxs = &groups[group];
        for (ii, &i) in idxs.iter().enumerate() {
            for &j in idxs.iter().skip(ii + 1) {
                let sa = &specs[i];
                let sb = &specs[j];
                let ka = sa.kind.clone().unwrap_or_default();
                let kb = sb.kind.clone().unwrap_or_default();
                if ka != kb {
                    continue;
                }
                let (x, y) = if sa.name <= sb.name {
                    (sa.name.as_str(), sb.name.as_str())
                } else {
                    (sb.name.as_str(), sa.name.as_str())
                };
                let pair_key = format!("{x}\u{0}{y}");
                if seen.contains(&pair_key) {
                    continue;
                }
                seen.insert(pair_key);
                let sim = doc_similarity(index, &sa.name, &sb.name);
                if sim >= DOC_SIM_FLOOR {
                    generated.push((
                        sa.name.clone(),
                        sb.name.clone(),
                        group.clone(),
                        js_to_fixed4(sim),
                    ));
                }
            }
        }
    }
    // oracle: doc_similarity desc, then a asc, then b asc
    generated.sort_by(|x, y| {
        y.3.partial_cmp(&x.3)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| x.0.cmp(&y.0))
            .then_with(|| x.1.cmp(&y.1))
    });

    let mut pairs: Vec<ConfusablePair> = Vec::new();
    for pin in &HAND_PINNED {
        let (Some(sa), Some(sb)) = (spec_by_name.get(pin.a), spec_by_name.get(pin.b)) else {
            continue; // a tool pruned upstream: the pin dies honestly
        };
        pairs.push(ConfusablePair {
            a: sa.name.clone(),
            b: sb.name.clone(),
            group: sa.group.clone().unwrap_or_default(),
            kind: sa.kind.clone().unwrap_or_default(),
            doc_similarity: js_to_fixed4(doc_similarity(index, &sa.name, &sb.name)),
            question: pin.question.to_string(),
            answers: pin
                .answers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            gain: pin.gain,
            evidence: pin.evidence.to_string(),
            source: "hand-pinned (arena failure class)".to_string(),
        });
    }
    for (a, b, group, sim) in generated.iter().take(24) {
        let (Some(qa), Some(qb)) = (spec_by_name.get(a.as_str()), spec_by_name.get(b.as_str()))
        else {
            continue;
        };
        let Some(q) = question_for(qa, qb) else {
            continue;
        };
        pairs.push(ConfusablePair {
            a: a.clone(),
            b: b.clone(),
            group: group.clone(),
            kind: qa.kind.clone().unwrap_or_default(),
            doc_similarity: *sim,
            question: q.question,
            answers: q.answers,
            gain: q.gain,
            evidence: format!(
                "mined: same-group {} documents similarity {}",
                serde_json::to_string(group).unwrap_or_default(),
                sim
            ),
            source: "mined (document twins)".to_string(),
        });
    }
    // hand-pinned first (oracle h = 0 for hand-pinned), then gain desc, then
    // a asc, then b asc (stable)
    pairs.sort_by(|x, y| {
        let hx = i32::from(x.source != "hand-pinned (arena failure class)");
        let hy = i32::from(y.source != "hand-pinned (arena failure class)");
        hx.cmp(&hy)
            .then_with(|| {
                y.gain
                    .partial_cmp(&x.gain)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| x.a.cmp(&y.a))
            .then_with(|| x.b.cmp(&y.b))
    });
    MinePairsResult {
        generator: "marfim.intel.confusables.minePairs".to_string(),
        doc_sim_floor: DOC_SIM_FLOOR,
        n_pairs: pairs.len(),
        pairs,
    }
}

/// The minePairs return shape.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct MinePairsResult {
    pub generator: String,
    pub doc_sim_floor: f64,
    pub n_pairs: usize,
    pub pairs: Vec<ConfusablePair>,
}

/// First committed pair fully present in the ranked candidates. The oracle
/// filters falsy candidates (null/undefined/""/0); the Rust surface carries
/// Option<String>, and empty strings are dropped exactly like falsy "".
fn pair_for(candidates: &[Option<String>], pairs: &[ConfusablePair]) -> Option<ConfusablePair> {
    let ranked: Vec<&str> = candidates
        .iter()
        .filter_map(|c| c.as_deref())
        .filter(|s| !s.is_empty())
        .collect();
    pairs
        .iter()
        .find(|p| ranked.contains(&p.a.as_str()) && ranked.contains(&p.b.as_str()))
        .cloned()
}

/// clarify(question, candidates, runner?) -> the pair's own ask when the
/// ranked candidates contain a mined pair, else the incoming question.
/// `runner` = None reads the committed artifact (the oracle's default);
/// `Some(pairs)` injects a pairs source (the oracle's `{pairs: [...]}` /
/// `{loadPairs: () => [...]}`).
pub fn clarify(
    question: &str,
    candidates: &[Option<String>],
    runner: Option<&[ConfusablePair]>,
) -> String {
    clarify_detail(question, candidates, runner).0
}

/// (question, upgraded, pair) — the full clarification result.
pub fn clarify_detail(
    question: &str,
    candidates: &[Option<String>],
    runner: Option<&[ConfusablePair]>,
) -> (String, bool, Option<ConfusablePair>) {
    let pairs: &[ConfusablePair] = match runner {
        Some(pairs) => pairs,
        None => load_pairs(),
    };
    match pair_for(candidates, pairs) {
        None => (question.to_string(), false, None),
        Some(pair) => (pair.question.clone(), true, Some(pair)),
    }
}

/// Reviewable "a <-> b -- source / ask:" lines over the committed artifact.
pub fn render_lines() -> Vec<String> {
    let pairs = load_pairs();
    let mut lines = vec![format!(
        "{} confusable pairs (committed artifact; rebuild with the miner):",
        pairs.len()
    )];
    for p in pairs {
        lines.push(format!("  {} <-> {}  -- {}", p.a, p.b, p.source));
        lines.push(format!("    ask: {}", p.question));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn committed_artifact_loads_with_27_pairs() {
        assert_eq!(load_pairs().len(), 27);
        assert_eq!(CONFUSABLE_PAIRS.len(), 27);
        assert_eq!(load_pairs()[0].a, "setAudioIncrement");
        assert_eq!(
            load_pairs()[0].question,
            "the step size for VOLUME, or for SCREEN BRIGHTNESS?"
        );
    }

    #[test]
    fn camel_split_matches_regex_semantics() {
        assert_eq!(camel_split(""), Vec::<String>::new());
        assert_eq!(
            camel_split("setTrayRecolour"),
            vec!["set", "tray", "recolour"]
        );
        // [A-Z]+ backtracks before a trailing lowercase: "ABCd" -> AB | cd
        assert_eq!(camel_split("ABCd"), vec!["ab", "cd"]);
        assert_eq!(camel_split("setTwinAa"), vec!["set", "twin", "aa"]);
        assert_eq!(camel_split("O'Brien"), vec!["o", "brien"]);
        assert_eq!(camel_split("123"), Vec::<String>::new());
        assert_eq!(
            camel_split("setEnabledState"),
            vec!["set", "enabled", "state"]
        );
    }

    #[test]
    fn question_scale_branch_and_camel_diff() {
        let a = ToolSpec {
            name: "setBarScale".into(),
            minimum: Some(0.1),
            maximum: Some(4.0),
            ..Default::default()
        };
        let b = ToolSpec {
            name: "setPreviewScale".into(),
            minimum: Some(0.0),
            maximum: Some(100.0),
            ..Default::default()
        };
        let q = question_for(&a, &b).unwrap_or_else(|| panic!("expected a draft"));
        assert_eq!(q.question, GENERIC_SCALE_QUESTION);
        assert_eq!(
            q.answers.get("scale multiplier").map(String::as_str),
            Some("setBarScale")
        );
        // identical names -> no differing atom -> None
        assert_eq!(question_for(&b, &b), None);
    }

    #[test]
    fn clarify_upgrades_only_when_pair_fully_present() {
        let candidates = vec![
            Some("setShowDashboard".to_string()),
            Some("setShowMedia".to_string()),
        ];
        let (q, upgraded, pair) = clarify_detail("which panel?", &candidates, None);
        assert!(upgraded);
        assert_eq!(q, "the dashboard setting, or the media setting?");
        assert!(pair.is_some());
        // empty-string candidates are falsy and never match
        let half = vec![Some("setShowMedia".to_string()), Some(String::new())];
        let (q2, upgraded2, pair2) = clarify_detail("which panel?", &half, None);
        assert!(!upgraded2);
        assert_eq!(q2, "which panel?");
        assert!(pair2.is_none());
    }

    #[test]
    fn js_to_fixed4_rounds_like_tofixed() {
        assert_eq!(js_to_fixed4(0.8184834633758628), 0.8185);
        assert_eq!(js_to_fixed4(0.0), 0.0);
        assert_eq!(js_to_fixed4(1.0), 1.0);
        assert_eq!(js_to_fixed4(0.55), 0.55);
    }
}
