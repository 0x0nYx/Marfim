//! Port of oracle `src/intel/brain/state.js` — the persistent model state
//! container: ONE JSON file holding every learner's plain-dict state under
//! its own key.
//!
//! `save` is atomic (write `<name>.tmp` then rename over the target) and
//! canonical: Python `json.dumps(state, indent=2, sort_keys=True)` byte
//! semantics — recursive key sort, `": "` / `",\n"` separators, ensure_ascii
//! `\uXXXX` escapes, `[]` / `{}` for empty containers, NaN/Infinity literals.
//! Same input -> same bytes.
//!
//! NARROWINGS (per the P4-B differential method — the pure parts are ported,
//! path resolution takes explicit inputs):
//! - `resolve_state_path` takes the explicit path, the MARFIM_BRAIN_STATE env
//!   value, and the home base as ARGUMENTS (the oracle reads
//!   `process.env`/`os.homedir()` itself; the fixture records the home it
//!   ran with so the replay is identical). Precedence: explicit > non-empty
//!   env > `<home>/.local/state/marfim-brain/state.json` (the oracle's
//!   `DEFAULT_STATE_PATH` join, pinned by the `DEFAULT_STATE_PATH` fixture
//!   line against Node's `path.join`).
//! - `load`/`save` are ported as explicit-path helpers
//!   ([`load_at`]/[`save_at`]); the env-reading wrappers stay at the call
//!   sites, which compose `resolve_state_path` + these.
//! - `py_json_stringify` operates on `serde_json::Value`, so the oracle's
//!   TypeError for Map/Set/class instances is unreachable (serde has no
//!   exotic JSON types) and `bigint` beyond u64/i64 cannot appear. JSON
//!   object key iteration follows serde_json's sorted `Map` order, so
//!   `sort_keys: false` cases in the fixtures use already-sorted keys (the
//!   oracle's insertion order is not reproducible through a JSON fixture —
//!   recorded in the fixture script too).
//! - `pyNumber` renders integral floats with JS `String(n)` semantics
//!   (bare digits below 1e21, exponent form at/above), and non-integral
//!   floats via `py_float_repr` (shortest round-trip, Python style).
//! - `escapeString` iterates code points; astral code points escape as
//!   surrogate pairs exactly like the oracle. Lone UTF-16 surrogates cannot
//!   occur in a Rust `str`.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// The default state path relative to the user's home (the oracle's
/// `join(homedir(), '.local', 'state', 'marfim-brain', 'state.json')`).
pub const DEFAULT_STATE_PATH_FROM_HOME: &str = ".local/state/marfim-brain/state.json";

/// The effective state path: explicit arg > MARFIM_BRAIN_STATE env > default
/// (oracle `resolveStatePath`, with the env and home supplied explicitly —
/// see the module narrowing). An EMPTY env value falls through, exactly like
/// the oracle's truthiness check.
pub fn resolve_state_path(explicit: Option<&str>, env_value: Option<&str>, home: &str) -> String {
    if let Some(p) = explicit {
        return p.to_string();
    }
    if let Some(env) = env_value {
        if !env.is_empty() {
            return env.to_string();
        }
    }
    PathBuf::from(home)
        .join(DEFAULT_STATE_PATH_FROM_HOME)
        .to_string_lossy()
        .to_string()
}

// ---------------------------------------------------------------------------
// Python json.dumps semantics (sort_keys, ensure_ascii, indent).
// ---------------------------------------------------------------------------

/// Inlined from oracle `src/intel/util/pyfloat.js` — `pyFloatRepr`: the
/// shortest decimal string that round-trips, in CPython's repr formatting
/// (fixed notation when -4 < decpt <= 16, else scientific with a signed
/// two-digit-exponent minimum; integral floats carry a trailing `.0`).
pub fn py_float_repr(x: f64) -> String {
    if x.is_nan() {
        return "nan".to_string();
    }
    if x == f64::INFINITY {
        return "inf".to_string();
    }
    if x == f64::NEG_INFINITY {
        return "-inf".to_string();
    }
    if x == 0.0 {
        return if x.is_sign_negative() {
            "-0.0".to_string()
        } else {
            "0.0".to_string()
        };
    }
    let neg = x < 0.0;
    let (digits, decpt) = shortest_digits(x.abs());
    let out = if decpt <= -4 || decpt > 16 {
        // scientific: d.ddde±XX (exponent = decpt - 1)
        let mant = if digits.len() > 1 {
            format!("{}.{}", &digits[..1], &digits[1..])
        } else {
            digits.clone()
        };
        let exp = decpt - 1;
        let esign = if exp < 0 { '-' } else { '+' };
        format!("{mant}e{esign}{:02}", exp.abs())
    } else if decpt <= 0 {
        format!("0.{}{digits}", "0".repeat((-decpt) as usize))
    } else if decpt as usize >= digits.len() {
        format!("{digits}{}.0", "0".repeat(decpt as usize - digits.len()))
    } else {
        format!(
            "{}.{}",
            &digits[..decpt as usize],
            &digits[decpt as usize..]
        )
    };
    if neg {
        format!("-{out}")
    } else {
        out
    }
}

/// Extract (digits, decpt) of the shortest round-trip form of |x|:
/// value = 0.<digits> * 10^decpt. Rust's `Display` is the shortest
/// round-trip decimal in fixed notation (matching the digits the oracle
/// extracts from JS `toExponential()`), so the digits are recovered by
/// position arithmetic.
fn shortest_digits(ax: f64) -> (String, i32) {
    let s = format!("{ax}");
    let (int_part, frac_part) = match s.split_once('.') {
        Some((a, b)) => (a, b),
        None => (s.as_str(), ""),
    };
    let all = format!("{int_part}{frac_part}");
    let first_nz = all.bytes().position(|b| b != b'0').unwrap_or(0);
    let significant = all[first_nz..].trim_end_matches('0');
    let digits = if significant.is_empty() {
        "0"
    } else {
        significant
    };
    let decpt = int_part.len() as i32 - first_nz as i32;
    (digits.to_string(), decpt)
}

/// Python `json.dumps` number rendering (oracle `pyNumber`): NaN/Infinity
/// literals, integral values bare (`3`, not `3.0` — JS `String(int)`
/// semantics, full digits below 1e21 and exponent form above), floats via
/// [`py_float_repr`].
fn py_number(n: &serde_json::Number) -> String {
    if let Some(i) = n.as_i64() {
        return i.to_string();
    }
    if let Some(u) = n.as_u64() {
        return u.to_string();
    }
    let f = n.as_f64().unwrap_or(f64::NAN);
    if f.is_nan() {
        return "NaN".to_string();
    }
    if f == f64::INFINITY {
        return "Infinity".to_string();
    }
    if f == f64::NEG_INFINITY {
        return "-Infinity".to_string();
    }
    if f.fract() == 0.0 {
        // JS Number.isInteger + String(): integral doubles render bare.
        // Below the f64-exact integer range this is the plain i64 digits;
        // above it, full digits until JS switches to exponent form (1e21).
        if f.abs() < 9.007_199_254_740_992e15 {
            return format!("{}", f as i64);
        }
        let (digits, decpt) = shortest_digits(f.abs());
        let body = if f.abs() >= 1e21 {
            let exp = decpt - 1;
            let mant = if digits.len() > 1 {
                format!("{}.{}", &digits[..1], &digits[1..])
            } else {
                digits.clone()
            };
            format!("{mant}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs())
        } else {
            format!("{digits}{}", "0".repeat(decpt as usize - digits.len()))
        };
        return if f < 0.0 { format!("-{body}") } else { body };
    }
    py_float_repr(f)
}

/// ensure_ascii string escaping (oracle `escapeString`): control chars and
/// non-ASCII become `\uXXXX` escapes (lowercase hex, surrogate pairs for
/// astral code points).
fn escape_string(s: &str) -> String {
    let mut out = String::from("\"");
    for ch in s.chars() {
        let code = ch as u32;
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            _ if !(0x20..=0x7e).contains(&code) => {
                if code > 0xffff {
                    let v = code - 0x10000;
                    let hi = 0xd800 + v / 0x400;
                    let lo = 0xdc00 + v % 0x400;
                    out.push_str(&format!("\\u{hi:04x}\\u{lo:04x}"));
                } else {
                    out.push_str(&format!("\\u{code:04x}"));
                }
            }
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

fn encode(value: &Value, indent: Option<i64>, level: i64, sort_keys: bool) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(b) => if *b { "true" } else { "false" }.to_string(),
        Value::Number(n) => py_number(n),
        Value::String(s) => escape_string(s),
        Value::Array(items) => {
            if items.is_empty() {
                return "[]".to_string();
            }
            let enc: Vec<String> = items
                .iter()
                .map(|v| encode(v, indent, level + 1, sort_keys))
                .collect();
            render_container(&enc, indent, level, "[", "]")
        }
        Value::Object(map) => {
            if map.is_empty() {
                return "{}".to_string();
            }
            // serde_json's Map is a sorted BTreeMap; with sort_keys the
            // oracle sorts identically. Without sort_keys the oracle emits
            // insertion order — the fixture narrowing keeps those keys
            // already sorted (see module docs).
            let mut keys: Vec<&String> = map.keys().collect();
            if sort_keys {
                keys.sort();
            }
            let enc: Vec<String> = keys
                .iter()
                .map(|k| {
                    format!(
                        "{}: {}",
                        escape_string(k),
                        encode(&map[*k], indent, level + 1, sort_keys)
                    )
                })
                .collect();
            render_container(&enc, indent, level, "{", "}")
        }
    }
}

fn render_container(
    items: &[String],
    indent: Option<i64>,
    level: i64,
    open: &str,
    close: &str,
) -> String {
    match indent {
        None => format!("{open}{}{close}", items.join(", ")),
        Some(width) => {
            let pad = " ".repeat(width.max(0) as usize);
            let inner_pad = pad.repeat((level + 1) as usize);
            let close_pad = pad.repeat(level as usize);
            let body: Vec<String> = items.iter().map(|it| format!("{inner_pad}{it}")).collect();
            format!("{open}\n{}\n{close_pad}{close}", body.join(",\n"))
        }
    }
}

/// Python ``json.dumps(value, indent=..., sort_keys=...)`` byte-exact
/// (oracle `pyJsonStringify`).
pub fn py_json_stringify(value: &Value, opts: JsonOpts) -> String {
    encode(value, opts.indent, 0, opts.sort_keys)
}

/// Stringify options mirroring the oracle's `{indent, sortKeys}`.
#[derive(Debug, Clone, Copy, Default)]
pub struct JsonOpts {
    /// `None` = compact (`", "` / `": "` separators); `Some(n)` = `n`-space
    /// indentation (negative values clamp to 0, as `Math.max(0, indent)`).
    pub indent: Option<i64>,
    pub sort_keys: bool,
}

/// Canonical sorted state bytes: `json.dumps(state, indent=2, sort_keys=True)`
/// (oracle `dumpsSorted`).
pub fn dumps_sorted(state: &Value) -> String {
    py_json_stringify(
        state,
        JsonOpts {
            indent: Some(2),
            sort_keys: true,
        },
    )
}

/// A fresh empty state (every learner persists under its own key) — oracle
/// `emptyState`.
pub fn empty_state() -> Value {
    json!({})
}

/// I/O surfaces of the state module (missing files, bad JSON, filesystem
/// errors).
#[derive(Debug)]
pub enum StateError {
    Io(std::io::Error),
    Json(serde_json::Error),
}

impl std::fmt::Display for StateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StateError::Io(e) => write!(f, "{e}"),
            StateError::Json(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for StateError {}

impl From<std::io::Error> for StateError {
    fn from(e: std::io::Error) -> Self {
        StateError::Io(e)
    }
}

impl From<serde_json::Error> for StateError {
    fn from(e: serde_json::Error) -> Self {
        StateError::Json(e)
    }
}

/// Atomic JSON write: mkdir -p the parent, write `<name>.tmp`, rename over
/// the target (oracle `writeJsonAtomic`).
pub fn write_json_atomic(path: &Path, text: &str) -> Result<(), StateError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let tmp = PathBuf::from(format!("{}.tmp", path.display()));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Load the brain state from an explicit path (`{}` when the file does not
/// exist yet) — oracle `load` with the path resolution narrowed out (see the
/// module docs).
pub fn load_at(path: &Path) -> Result<Value, StateError> {
    if !path.exists() {
        return Ok(json!({}));
    }
    let text = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&text)?)
}

/// Save the brain state atomically (tmp + rename), canonical bytes — oracle
/// `save` with the path resolution narrowed out.
pub fn save_at(state: &Value, path: &Path) -> Result<(), StateError> {
    write_json_atomic(path, &dumps_sorted(state))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dumps_sorted_matches_python_json_dumps() {
        let state = json!({
            "b": 1,
            "a": 2.5,
            "list": [3, 1, 2],
            "nested": { "z": null, "y": true }
        });
        let out = dumps_sorted(&state);
        let expected = "{\n  \"a\": 2.5,\n  \"b\": 1,\n  \"list\": [\n    3,\n    1,\n    2\n  ],\n  \"nested\": {\n    \"y\": true,\n    \"z\": null\n  }\n}";
        assert_eq!(out, expected);
    }

    #[test]
    fn ensure_ascii_escapes_and_surrogate_pairs() {
        assert_eq!(
            py_json_stringify(&json!("héllo"), JsonOpts::default()),
            "\"h\\u00e9llo\""
        );
        // U+1D11E (musical symbol) escapes as a surrogate pair
        assert_eq!(
            py_json_stringify(&json!("𝄞"), JsonOpts::default()),
            "\"\\ud834\\udd1e\""
        );
        assert_eq!(
            py_json_stringify(&json!("a\u{1}\t\"\\\n"), JsonOpts::default()),
            "\"a\\u0001\\t\\\"\\\\\\n\""
        );
    }

    #[test]
    fn py_number_renders_python_literals() {
        let num = |v: serde_json::Value| py_number(v.as_number().expect("number"));
        assert_eq!(num(serde_json::json!(3)), "3");
        assert_eq!(num(serde_json::json!(3.5)), "3.5");
        assert_eq!(num(serde_json::json!(0.1)), "0.1");
        assert_eq!(num(serde_json::json!(100.0)), "100");
        // integral doubles beyond the exact-integer range keep JS String()
        // semantics: full digits below 1e21
        assert_eq!(num(serde_json::json!(1e16)), "10000000000000000");
    }

    #[test]
    fn py_float_repr_matches_cpython() {
        assert_eq!(py_float_repr(0.0), "0.0");
        assert_eq!(py_float_repr(-0.0), "-0.0");
        assert_eq!(py_float_repr(1.5), "1.5");
        assert_eq!(py_float_repr(1e15), "1000000000000000.0");
        assert_eq!(py_float_repr(1e16), "1e+16");
        assert_eq!(py_float_repr(1e-7), "1e-07");
        assert_eq!(py_float_repr(0.30000000000000004), "0.30000000000000004");
        assert_eq!(py_float_repr(f64::NAN), "nan");
        assert_eq!(py_float_repr(f64::INFINITY), "inf");
    }

    #[test]
    fn compact_mode_uses_python_separators() {
        assert_eq!(
            py_json_stringify(&json!([1, 2.5, "x", null, true]), JsonOpts::default()),
            "[1, 2.5, \"x\", null, true]"
        );
        assert_eq!(py_json_stringify(&json!({}), JsonOpts::default()), "{}");
        assert_eq!(py_json_stringify(&json!([]), JsonOpts::default()), "[]");
    }

    #[test]
    fn negative_indent_clamps_to_zero() {
        let opts = JsonOpts {
            indent: Some(-1),
            sort_keys: true,
        };
        assert_eq!(
            py_json_stringify(&json!({"a": [1]}), opts),
            "{\n\"a\": [\n1\n]\n}"
        );
    }

    #[test]
    fn resolve_precedence_explicit_env_then_default() {
        assert_eq!(
            resolve_state_path(Some("/explicit.json"), Some("/env.json"), "/home/t"),
            "/explicit.json"
        );
        assert_eq!(
            resolve_state_path(None, Some("/env.json"), "/home/t"),
            "/env.json"
        );
        // empty env is falsy in the oracle
        assert_eq!(
            resolve_state_path(None, Some(""), "/home/t"),
            "/home/t/.local/state/marfim-brain/state.json"
        );
        assert_eq!(
            resolve_state_path(None, None, "/home/t"),
            "/home/t/.local/state/marfim-brain/state.json"
        );
    }

    #[test]
    fn atomic_write_and_load_roundtrip() {
        let dir =
            std::env::temp_dir().join(format!("marfim-brain-state-test-{}", std::process::id()));
        let path = dir.join("nested").join("state.json");
        let state = json!({ "learner": { "alpha": 1.5, "counts": { "a": 2 } } });
        save_at(&state, &path).unwrap_or_else(|e| panic!("save: {e}"));
        // tmp file is gone, target holds canonical bytes
        assert!(!path.with_file_name("state.json.tmp").exists());
        let loaded = load_at(&path).unwrap_or_else(|e| panic!("load: {e}"));
        assert_eq!(loaded, state);
        let bytes = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read: {e}"));
        assert_eq!(bytes, dumps_sorted(&state));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_missing_file_is_empty_state() {
        let missing = std::env::temp_dir().join("marfim-brain-state-missing-does-not-exist.json");
        assert_eq!(
            load_at(&missing).unwrap_or_else(|e| panic!("{e}")),
            empty_state()
        );
        assert_eq!(empty_state(), json!({}));
    }
}
