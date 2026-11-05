//! D6/G7 enforcement: no `.unwrap()`, `.expect(`, `panic!(`, `todo!` or
//! `unimplemented!` in NON-TEST source of any workspace crate. `#[cfg(test)]`
//! module bodies are stripped by brace matching before the scan (tests may
//! panic freely). `unreachable!` is allowed — it asserts a proven invariant,
//! it is not a placeholder. The allowlist below is deliberately tiny; every
//! entry carries a justification. Adding a new entry requires a reason.

use std::path::{Path, PathBuf};

const FORBIDDEN: &[&str] = &[
    ".unwrap()",
    ".expect(",
    "panic!(",
    "todo!",
    "unimplemented!",
];

/// (file name suffix, needle, justification)
const ALLOWLIST: &[(&str, &str, &str)] = &[(
    "tools_host.rs",
    ".expect(",
    "static tool table registered into a fresh registry; uniqueness is \
         proven by the every_builtin_registers_cleanly test — a duplicate \
         name is a programmer error that must fail loudly at startup",
)];

fn workspace_crates() -> Vec<PathBuf> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")); // crates/marfim-core
    let crates_dir = manifest.parent().expect("crates dir").to_path_buf();
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&crates_dir).expect("crates dir readable") {
        let p = entry.expect("entry").path();
        let src = p.join("src");
        if src.is_dir() {
            out.push(src);
        }
    }
    out.sort();
    out
}

/// Strip `#[cfg(test)] mod ... { ... }` bodies (brace-matched) so test-only
/// panics do not trip the scan.
fn strip_test_modules(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut lines = text.lines().peekable();
    let mut in_test_block = false;
    let mut depth: isize = 0;
    while let Some(line) = lines.next() {
        if !in_test_block {
            let trimmed = line.trim_start();
            if trimmed.starts_with("#[cfg(test)]") {
                // hold the attribute; look ahead for the mod line
                if let Some(next) = lines.peek() {
                    if next.trim_start().starts_with("mod ") {
                        in_test_block = true;
                        depth =
                            next.matches('{').count() as isize - next.matches('}').count() as isize;
                        continue; // skip attribute + mod line
                    }
                }
                out.push_str(line);
                out.push('\n');
                continue;
            }
            out.push_str(line);
            out.push('\n');
        } else {
            depth += line.matches('{').count() as isize;
            depth -= line.matches('}').count() as isize;
            if depth <= 0 {
                in_test_block = false;
            }
        }
    }
    out
}

#[test]
fn no_unwrap_expect_or_placeholder_macros_in_non_test_src() {
    let mut violations = Vec::new();
    let mut scanned_files = 0usize;
    let mut sanity_seen_register_builtins = false;
    for src_dir in workspace_crates() {
        for file in walk_rs(&src_dir) {
            let text = std::fs::read_to_string(&file).expect("read src file");
            let code = strip_test_modules(&text);
            // scanner sanity: stripping must not eat non-test code
            if code.contains("pub fn register_builtins") {
                sanity_seen_register_builtins = true;
            }
            if !code.trim().is_empty() {
                scanned_files += 1;
            }
            for needle in FORBIDDEN {
                if let Some(pos) = code.find(needle) {
                    let rel = file.display().to_string();
                    let allowed = ALLOWLIST
                        .iter()
                        .any(|(suffix, n, _)| rel.ends_with(suffix) && *n == *needle);
                    if !allowed {
                        let line_no = code[..pos].matches('\n').count() + 1;
                        let snippet: String = code[pos..].chars().take(60).collect();
                        violations.push(format!("{rel}:{line_no}: `{needle}` → {snippet}"));
                    }
                }
            }
        }
    }
    assert!(
        scanned_files >= 40,
        "scanner stripped too much: only {scanned_files} non-empty src files"
    );
    assert!(
        sanity_seen_register_builtins,
        "scanner sanity failed: non-test code not visible after stripping"
    );
    assert!(
        violations.is_empty(),
        "D6/G7 violation(s) — fallible paths must propagate errors, not panic:\n{}",
        violations.join("\n")
    );
}

fn walk_rs(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).expect("read_dir") {
            let p = entry.expect("entry").path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|e| e.to_str()) == Some("rs") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}
