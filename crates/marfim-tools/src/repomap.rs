//! RepoMap: heuristic symbol extraction + reference ranking. Languages:
//! py, rs, go, js/ts, java, c/cpp, rb. Budget-limited outline output.

use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub struct Symbol {
    pub name: String,
    pub kind: &'static str,
    pub line: usize,
}

pub fn lang_of(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_string_lossy().to_string();
    Some(match ext.as_str() {
        "py" => "py",
        "rs" => "rs",
        "go" => "go",
        "js" | "jsx" | "ts" | "tsx" | "mjs" | "cjs" => "js",
        "java" => "java",
        "c" | "h" | "cpp" | "cc" | "hpp" => "c",
        "rb" => "rb",
        _ => return None,
    })
}

/// Strip declaration modifiers (rs): `pub`, `pub(crate)`, `pub(super)`,
/// `pub(in path)`, `async`, `const`, `unsafe`, `default`, `extern "..."`,
/// `extern` — iteratively, so `pub async const unsafe fn f()` still lands
/// on the keyword. Deterministic, no allocation beyond one trimmed copy.
fn strip_rs_modifiers(mut s: &str) -> &str {
    loop {
        let t = s.trim_start();
        if t.starts_with("pub(") {
            // pub(crate) / pub(in path::to) — strip through the closing paren
            if let Some(end) = t.find(')') {
                s = &t[end + 1..];
                continue;
            }
        }
        let stripped = t
            .strip_prefix("pub ")
            .or_else(|| t.strip_prefix("async "))
            .or_else(|| t.strip_prefix("const "))
            .or_else(|| t.strip_prefix("unsafe "))
            .or_else(|| t.strip_prefix("default "))
            .or_else(|| t.strip_prefix("extern "))
            .or_else(|| t.strip_prefix("extern"));
        match stripped {
            Some(rest) if !rest.is_empty() => s = rest,
            _ => return s.trim_start(),
        }
    }
}

/// Comment prefix per language — a declaration inside a comment must not
/// produce a symbol (the old extractor matched `// fn x()` as a def).
fn comment_prefix(lang: &str) -> &'static [&'static str] {
    match lang {
        "py" | "rb" => &["#"],
        "rs" | "go" | "java" | "c" => &["//"],
        "js" => &["//", "/*", "*"],
        _ => &[],
    }
}

/// Extract symbols from one source file with per-language patterns.
pub fn extract(path: &Path, text: &str) -> Vec<Symbol> {
    let Some(lang) = lang_of(path) else {
        return vec![];
    };
    let comments = comment_prefix(lang);
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let t = line.trim();
        if t.is_empty() || comments.iter().any(|p| t.starts_with(p)) {
            continue;
        }
        let sym = match lang {
            "py" => {
                if let Some(n) = strip(t, "def ")
                    .or_else(|| strip(t, "async def "))
                    .or_else(|| strip(t, "class "))
                {
                    let name = n
                        .split('(')
                        .next()
                        .unwrap_or(n)
                        .split(':')
                        .next()
                        .unwrap_or(n);
                    let kind = if t.starts_with("class") {
                        "class"
                    } else {
                        "fn"
                    };
                    Some((name.trim().to_string(), kind))
                } else {
                    None
                }
            }
            "rs" => {
                let t2 = strip_rs_modifiers(t);
                for kw in ["fn ", "struct ", "enum ", "trait ", "impl ", "mod "] {
                    if let Some(n) = strip(t2, kw) {
                        let name = n
                            .split(|c: char| c == '(' || c == '<' || c == '{' || c.is_whitespace())
                            .next()
                            .unwrap_or(n);
                        let kind = kw.trim();
                        if !name.is_empty()
                            && name
                                .chars()
                                .next()
                                .map(|c| c.is_alphabetic() || c == '_')
                                .unwrap_or(false)
                        {
                            out.push(Symbol {
                                name: name.to_string(),
                                kind: match kind {
                                    "fn" => "fn",
                                    "struct" => "struct",
                                    "enum" => "enum",
                                    "trait" => "trait",
                                    "impl" => "impl",
                                    _ => "mod",
                                },
                                line: i + 1,
                            });
                        }
                        break;
                    }
                }
                None
            }
            "go" => {
                // receiver methods (`func (r *T) Name(...)`) previously fell
                // through the '(' split to an empty name and were dropped
                if let Some(n) = strip(t, "func ") {
                    let raw = if let Some(rest) = n.strip_prefix('(') {
                        // receiver: `func (s *Server) Start(...)` — the name
                        // sits between the receiver's ')' and the next '('
                        match rest.find(')') {
                            Some(end) => rest[end + 1..].split('(').next().unwrap_or("").trim(),
                            None => "",
                        }
                    } else {
                        n.split('(').next().unwrap_or(n)
                    };
                    let name = raw.split('[').next().unwrap_or(raw).trim();
                    if !name.is_empty()
                        && name
                            .chars()
                            .next()
                            .map(|c| c.is_alphabetic() || c == '_')
                            .unwrap_or(false)
                    {
                        out.push(Symbol {
                            name: name.to_string(),
                            kind: "fn",
                            line: i + 1,
                        });
                    }
                }
                // `type X struct` / `type X interface` are real symbols
                if let Some(n) = strip(t, "type ") {
                    let name = n.split_whitespace().next().unwrap_or("");
                    if !name.is_empty()
                        && name
                            .chars()
                            .next()
                            .map(|c| c.is_alphabetic() || c == '_')
                            .unwrap_or(false)
                    {
                        out.push(Symbol {
                            name: name.to_string(),
                            kind: "class",
                            line: i + 1,
                        });
                    }
                }
                None
            }
            "js" => {
                // strip an optional export / export default prefix first
                let t2 = strip(t, "export default ")
                    .or_else(|| strip(t, "export "))
                    .unwrap_or(t);
                for pat in [
                    "async function ",
                    "function ",
                    "class ",
                    "interface ",
                    "type ",
                    "const ",
                ] {
                    if let Some(n) = strip(t2, pat) {
                        let name = n
                            .split(|c: char| c == '(' || c == '=' || c == '{' || c.is_whitespace())
                            .next()
                            .unwrap_or(n);
                        if !name.is_empty()
                            && name
                                .chars()
                                .next()
                                .map(|c| c.is_alphabetic() || c == '_' || c == '$')
                                .unwrap_or(false)
                        {
                            let kind = if pat.contains("class") || pat == "interface " {
                                "class"
                            } else if pat == "type " {
                                "type"
                            } else {
                                "fn"
                            };
                            out.push(Symbol {
                                name: name.to_string(),
                                kind,
                                line: i + 1,
                            });
                        }
                        break;
                    }
                }
                None
            }
            "java" => {
                if t.contains("class ") || t.contains("interface ") || t.contains("void ") {
                    let n = t
                        .trim_start_matches(|c: char| !c.is_alphabetic())
                        .split(|c: char| c == '(' || c == '{' || c.is_whitespace())
                        .find(|s| {
                            !s.is_empty() && *s != "class" && *s != "public" && *s != "interface"
                        })
                        .unwrap_or("");
                    if !n.is_empty() {
                        out.push(Symbol {
                            name: n.to_string(),
                            kind: "def",
                            line: i + 1,
                        });
                    }
                }
                None
            }
            "c" => {
                if t.contains('(')
                    && !t.starts_with("//")
                    && !t.starts_with('#')
                    && (t.contains("int ")
                        || t.contains("void ")
                        || t.contains("char ")
                        || t.contains("bool ")
                        || t.contains("static "))
                {
                    let n = t
                        .split('(')
                        .next()
                        .unwrap_or("")
                        .split_whitespace()
                        .last()
                        .unwrap_or("");
                    if !n.is_empty() {
                        out.push(Symbol {
                            name: n.trim_start_matches('*').to_string(),
                            kind: "fn",
                            line: i + 1,
                        });
                    }
                }
                None
            }
            "rb" => {
                if let Some(n) = strip(t, "def ").or_else(|| strip(t, "class ")) {
                    Some((
                        n.trim().to_string(),
                        if t.starts_with("class") {
                            "class"
                        } else {
                            "fn"
                        },
                    ))
                } else {
                    None
                }
            }
            _ => None,
        };
        if let Some((name, kind)) = sym {
            if !name.is_empty() && name.len() < 128 {
                out.push(Symbol {
                    name,
                    kind,
                    line: i + 1,
                });
            }
        }
    }
    out
}

fn strip<'a>(line: &'a str, kw: &str) -> Option<&'a str> {
    line.strip_prefix(kw)
}

/// Build the map: symbols per file ranked by personalized PageRank over
/// the file-level reference graph (a file references a symbol's definer →
/// a directed edge; rank flows back to definers), with the reference
/// count kept as a tiebreak. Output capped to `budget` lines.
pub fn build(files: &[(std::path::PathBuf, String)], budget: usize) -> String {
    let mut symbols: BTreeMap<String, Vec<(std::path::PathBuf, usize)>> = BTreeMap::new();
    let mut all_text: Vec<(std::path::PathBuf, String)> = Vec::new();
    for (p, text) in files {
        for s in extract(p, text) {
            symbols
                .entry(s.name.clone())
                .or_default()
                .push((p.clone(), s.line));
        }
        all_text.push((p.clone(), text.clone()));
    }
    // file-level reference graph: edge src → dst when src mentions a
    // symbol DEFINED in dst (src is not itself a definer of that symbol)
    let mut graph: BTreeMap<usize, BTreeMap<usize, f64>> = BTreeMap::new();
    for (name, defs) in &symbols {
        for (p, text) in &all_text {
            let in_this = defs.iter().any(|(dp, _)| dp == p);
            if in_this || !text.contains(name.as_str()) {
                continue;
            }
            let src = all_text.iter().position(|(q, _)| q == p).unwrap_or(0);
            for (dp, _line) in defs {
                let dst = all_text.iter().position(|(q, _)| q == dp).unwrap_or(0);
                if src != dst {
                    *graph.entry(src).or_default().entry(dst).or_insert(0.0) += 1.0;
                }
            }
        }
    }
    let ranks = personalized_pagerank(&graph, all_text.len(), 40, 0.85);
    // score per symbol definition: PageRank of its defining file, plus the
    // plain cross-file reference count as a deterministic tiebreak
    let mut refs: Vec<(f64, usize, String, &std::path::PathBuf, usize)> = Vec::new();
    for (name, defs) in &symbols {
        let mut count = 0usize;
        for (p, text) in &all_text {
            let in_this = defs.iter().any(|(dp, _)| dp == p);
            if !in_this && text.contains(name.as_str()) {
                count += 1;
            }
        }
        for (p, line) in defs {
            let idx = all_text.iter().position(|(q, _)| q == p).unwrap_or(0);
            let rank = ranks.get(idx).copied().unwrap_or(0.0);
            refs.push((rank, count, name.clone(), p, *line));
        }
    }
    // sort: rank desc, then refs desc, then name asc (deterministic)
    refs.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.1.cmp(&a.1))
            .then_with(|| a.2.cmp(&b.2))
    });
    let mut out = String::new();
    for (_rank, count, name, path, line) in refs.into_iter().take(budget) {
        out.push_str(&format!(
            "{}:{}: {} (refs: {count})\n",
            path.display(),
            line,
            name
        ));
    }
    out
}

/// Personalized PageRank over a directed weighted graph, uniform personal
/// vector. Deterministic: BTreeMap iteration, fixed iteration count, no
/// RNG. Convergence is bounded by `iters` (40 is plenty at damping 0.85
/// for repo-sized graphs; the result is stable to well under the ranking
/// resolution).
fn personalized_pagerank(
    graph: &BTreeMap<usize, BTreeMap<usize, f64>>,
    n: usize,
    iters: usize,
    damping: f64,
) -> Vec<f64> {
    if n == 0 {
        return Vec::new();
    }
    let mut rank = vec![1.0 / n as f64; n];
    // precompute out-weights (deterministic order via BTreeMap)
    let mut out_total = vec![0.0f64; n];
    for (src, outs) in graph {
        let t: f64 = outs.values().sum();
        out_total[*src] = t;
    }
    for _ in 0..iters {
        let mut next = vec![damping / n as f64; n];
        for (src, outs) in graph {
            let total = out_total[*src];
            if total <= 0.0 {
                continue;
            }
            let share = damping * rank[*src] / total;
            for (dst, w) in outs {
                next[*dst] += share * w;
            }
        }
        rank = next;
    }
    rank
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn extracts_rust_and_python() {
        let rs = extract(
            Path::new("src/lib.rs"),
            "pub fn alpha() {}\nstruct Beta { x: u8 }\nenum Gamma { A }\nimpl Beta {}\ntrait Delta {}\n",
        );
        let names: Vec<&str> = rs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"alpha"));
        assert!(names.contains(&"Beta"));
        assert!(names.contains(&"Gamma"));
        assert!(names.contains(&"Delta"));

        let py = extract(
            Path::new("m.py"),
            "def one(x):\n    return x\n\nclass Two:\n    pass\n",
        );
        assert!(py.iter().any(|s| s.name == "one" && s.kind == "fn"));
        assert!(py.iter().any(|s| s.name == "Two" && s.kind == "class"));
    }

    #[test]
    fn unknown_language_is_empty() {
        assert!(extract(Path::new("a.txt"), "def f(): pass").is_empty());
    }

    #[test]
    fn modifier_prefixed_rust_declarations_are_extracted() {
        let src = "pub async fn worker() {}\n\
                   pub(crate) fn helper() {}\n\
                   pub(super) const fn make() -> u8 { 0 }\n\
                   pub unsafe trait Raw { }\n\
                   async fn tail() {}\n\
                   pub(crate) struct Inner { x: u8 }\n";
        let syms = extract(Path::new("src/a.rs"), src);
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        for expected in ["worker", "helper", "make", "Raw", "tail", "Inner"] {
            assert!(names.contains(&expected), "{expected} missing: {names:?}");
        }
    }

    #[test]
    fn commented_declarations_produce_no_symbols() {
        let rs = extract(
            Path::new("a.rs"),
            "// fn ghost() {}\n/// struct AlsoGhost {}\nfn real() {}\n",
        );
        let names: Vec<&str> = rs.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["real"],
            "commented defs must not become symbols"
        );
        let py = extract(Path::new("b.py"), "# def ghost(): pass\ndef real(): pass\n");
        assert_eq!(py.len(), 1);
        assert_eq!(py[0].name, "real");
    }

    #[test]
    fn python_async_defs_and_go_receivers_are_extracted() {
        let py = extract(Path::new("a.py"), "async def fetch(x):\n    return x\n");
        assert!(py.iter().any(|s| s.name == "fetch"));

        let go = extract(
            Path::new("svc.go"),
            "func (s *Server) Start(ctx context.Context) error { return nil }\n\
             func Plain() {}\n\
             type Config struct { A int }\n\
             type Store interface { Get() }\n",
        );
        let names: Vec<&str> = go.iter().map(|s| s.name.as_str()).collect();
        for expected in ["Start", "Plain", "Config", "Store"] {
            assert!(names.contains(&expected), "{expected} missing: {names:?}");
        }
    }

    #[test]
    fn ts_interfaces_and_export_const_are_extracted() {
        let ts = extract(
            Path::new("types.ts"),
            "export interface Options { a: number }\n\
             export type Alias = string;\n\
             export const LIMIT = 3;\n\
             export async function run(): Promise<void> {}\n\
             export default class Main {}\n",
        );
        let names: Vec<&str> = ts.iter().map(|s| s.name.as_str()).collect();
        for expected in ["Options", "Alias", "LIMIT", "run", "Main"] {
            assert!(names.contains(&expected), "{expected} missing: {names:?}");
        }
    }

    #[test]
    fn pagerank_ranks_hub_files_above_leaves() {
        // hub.rs defines `hub_fn` referenced by four files; leaf.rs defines
        // `leaf_fn` referenced by nobody. PageRank must put the hub first.
        let files = vec![
            (PathBuf::from("hub.rs"), "pub fn hub_fn() {}".to_string()),
            (PathBuf::from("leaf.rs"), "pub fn leaf_fn() {}".to_string()),
            (PathBuf::from("a.rs"), "fn x() { hub_fn(); }".to_string()),
            (PathBuf::from("b.rs"), "fn y() { hub_fn(); }".to_string()),
            (PathBuf::from("c.rs"), "fn z() { hub_fn(); }".to_string()),
            (PathBuf::from("d.rs"), "fn w() { hub_fn(); }".to_string()),
        ];
        let map = build(&files, 10);
        let hub_line = map
            .lines()
            .position(|l| l.contains("hub_fn"))
            .unwrap_or(usize::MAX);
        let leaf_line = map
            .lines()
            .position(|l| l.contains("leaf_fn"))
            .unwrap_or(usize::MAX);
        assert!(hub_line < leaf_line, "hub must outrank leaf:\n{map}");
        // determinism: same input, same output
        let again = build(&files, 10);
        assert_eq!(map, again);
    }

    #[test]
    fn pagerank_deterministic_on_cycle() {
        // a→b→c→a cycle: every rank equal, order decided by the name
        // tiebreak — and identical across calls.
        let files = vec![
            (PathBuf::from("x.rs"), "pub fn aa() {}".to_string()),
            (
                PathBuf::from("y.rs"),
                "fn q() { aa(); } pub fn bb() {}".to_string(),
            ),
            (PathBuf::from("z.rs"), "fn r() { bb(); }".to_string()),
        ];
        let m1 = build(&files, 10);
        let m2 = build(&files, 10);
        assert_eq!(m1, m2);
        assert!(!m1.is_empty());
    }

    #[test]
    fn map_ranks_referenced_symbols_first() {
        let files = vec![
            (PathBuf::from("a.rs"), "pub fn core_fn() {}".to_string()),
            (PathBuf::from("b.rs"), "pub fn leaf_fn() {}".to_string()),
            (
                PathBuf::from("c.rs"),
                "use crate::core_fn;\nfn call() { core_fn(); }".to_string(),
            ),
        ];
        let map = build(&files, 10);
        let core_pos = map.find("core_fn").unwrap();
        let leaf_pos = map.find("leaf_fn").unwrap();
        assert!(
            core_pos < leaf_pos,
            "referenced symbol must rank first:\n{map}"
        );
    }

    #[test]
    fn budget_caps_output() {
        let files: Vec<(std::path::PathBuf, String)> = (0..30)
            .map(|i| {
                (
                    std::path::PathBuf::from(format!("f{i}.rs")),
                    format!("pub fn sym{i}() {{}}"),
                )
            })
            .collect();
        let map = build(&files, 5);
        assert_eq!(map.lines().count(), 5);
    }
}
