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

/// Extract symbols from one source file with per-language patterns.
pub fn extract(path: &Path, text: &str) -> Vec<Symbol> {
    let Some(lang) = lang_of(path) else {
        return vec![];
    };
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let t = line.trim();
        let sym = match lang {
            "py" => {
                if let Some(n) = strip(t, "def ").or_else(|| strip(t, "class ")) {
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
                let t2 = t.strip_prefix("pub ").unwrap_or(t);
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
            "go" => strip(t, "func ").map(|n| {
                let name = n.split('(').next().unwrap_or(n);
                (name.trim().to_string(), "fn")
            }),
            "js" => {
                for pat in [
                    "function ",
                    "class ",
                    "const ",
                    "export function ",
                    "export class ",
                ] {
                    if let Some(n) = strip(t, pat) {
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
                            out.push(Symbol {
                                name: name.to_string(),
                                kind: if pat.contains("class") { "class" } else { "fn" },
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

/// Build the map: symbols per file ranked by how often their names are
/// referenced by OTHER files. Output capped to `budget` lines.
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
    // reference counts: name mentioned in other files
    let mut refs: Vec<(usize, String, &std::path::PathBuf, usize)> = Vec::new();
    for (name, defs) in &symbols {
        let mut count = 0usize;
        for (p, text) in &all_text {
            let in_this = defs.iter().any(|(dp, _)| dp == p);
            if !in_this && text.contains(name.as_str()) {
                count += 1;
            }
        }
        for (p, line) in defs {
            refs.push((count, name.clone(), p, *line));
        }
    }
    refs.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let mut out = String::new();
    for (count, name, path, line) in refs.into_iter().take(budget) {
        out.push_str(&format!(
            "{}:{}: {} (refs: {count})\n",
            path.display(),
            line,
            name
        ));
    }
    out
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
