//! G14 — FFI hygiene (W4.1). The gate battery runs this as
//! `cargo test -p marfim-ffi --test ffi_hygiene`.
//!
//! Enforced invariants:
//! 1. every workspace crate's crate root (lib.rs / main.rs) carries
//!    `#![forbid(unsafe_code)]` — EXCEPT marfim-ffi, the single sanctioned
//!    holder of unsafe (DISTILLATION §FORMER-REJECTS: catch_unwind at the
//!    FFI boundary only, core stays forbid(unsafe));
//! 2. every `extern "C"` fn in marfim-ffi wraps its body in `catch_unwind`
//!    (panic containment at the boundary, panic can never cross);
//! 3. every `unsafe {` block or unsafe call site in marfim-ffi non-test
//!    source carries a `// SAFETY:` comment in the preceding comment run.

use std::path::{Path, PathBuf};

const FFI_CRATE: &str = "marfim-ffi";

fn workspace_crate_roots() -> Vec<(String, PathBuf)> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")); // crates/marfim-ffi
    let crates_dir = manifest.parent().expect("crates dir").to_path_buf();
    let mut out = Vec::new();
    let entries = std::fs::read_dir(&crates_dir).expect("crates dir readable");
    for entry in entries {
        let entry = entry.expect("entry");
        let dir = entry.path();
        for root in ["src/lib.rs", "src/main.rs"] {
            let p = dir.join(root);
            if p.is_file() {
                let name = dir
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                out.push((name, p));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn every_crate_except_the_ffi_boundary_forbids_unsafe() {
    let roots = workspace_crate_roots();
    assert!(roots.len() >= 10, "workspace crates discovered: {roots:?}");
    let mut ffi_seen = false;
    for (name, path) in &roots {
        let text = std::fs::read_to_string(path).expect("crate root readable");
        let forbids = text.contains("#![forbid(unsafe_code)]");
        if name == FFI_CRATE {
            ffi_seen = true;
            assert!(
                !forbids,
                "marfim-ffi must NOT forbid unsafe (it is the sanctioned boundary)"
            );
        } else {
            assert!(
                forbids,
                "crate {name} ({}) must carry #![forbid(unsafe_code)]",
                path.display()
            );
        }
    }
    assert!(ffi_seen, "marfim-ffi crate root not found");
}

/// Segment marfim-ffi's non-test, non-comment source into extern-fn chunks
/// and require `catch_unwind` in every chunk (the wrapper pattern is
/// structural, not best-effort). Comment lines are dropped first so doc
/// mentions of the pattern cannot satisfy a scan.
#[test]
fn every_extern_c_fn_wraps_catch_unwind() {
    let src = code_only(&non_test_source());
    let mut hits = 0;
    for (idx, _) in src.match_indices("extern \"C\" fn") {
        let body_start = idx + "extern \"C\" fn".len();
        let rest = &src[body_start..];
        // The fn body extends to the matching closing brace of its
        // definition; a segment scan up to the next extern fn (or EOF)
        // over-approximates only if a later extern fn would satisfy an
        // earlier one — prevented by requiring the catch BEFORE that next
        // extern fn begins.
        let next_extern = rest.find("extern \"C\" fn").unwrap_or(rest.len());
        let segment = &rest[..next_extern];
        assert!(
            segment.contains("catch_unwind"),
            "extern \"C\" fn at byte {idx} does not wrap catch_unwind"
        );
        hits += 1;
    }
    assert!(hits >= 4, "expected at least 4 extern fns, saw {hits}");
}

#[test]
fn every_unsafe_block_carries_a_safety_comment() {
    // Scan the WITH-comments source: the SAFETY marker is itself a comment,
    // so stripping comments here would delete the evidence it looks for.
    let src = non_test_source();
    for (idx, _) in src.match_indices("unsafe {") {
        // The match can land mid-line (`let x = unsafe { … }`); start the
        // backward comment run at the START of that line so the comments
        // above it are actually reachable.
        let line_start = src[..idx].rfind('\n').map(|p| p + 1).unwrap_or(0);
        let run_start = src[..line_start]
            .lines()
            .rev()
            .take_while(|l| {
                let t = l.trim_start();
                t.starts_with("//") || t.is_empty()
            })
            .collect::<Vec<_>>();
        let has_safety = run_start
            .iter()
            .any(|l| l.trim_start().starts_with("// SAFETY:"));
        assert!(
            has_safety,
            "unsafe block at byte {idx} lacks a `// SAFETY:` comment in its preceding comment run"
        );
    }
}

/// Drop `//`-comment lines so documentation prose can never satisfy a
/// structural scan.
fn code_only(text: &str) -> String {
    text.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// marfim-ffi source with `#[cfg(test)] mod` bodies stripped (same brace
/// matching as the G1 hygiene scanner), so test-only unsafe (which also
/// carries SAFETY comments today) never inflates the count.
fn non_test_source() -> String {
    let lib = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs");
    let text = std::fs::read_to_string(&lib).expect("lib.rs readable");
    strip_test_modules(&text)
}

fn strip_test_modules(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut lines = text.lines().peekable();
    let mut in_test_block = false;
    let mut depth: isize = 0;
    while let Some(line) = lines.next() {
        if !in_test_block {
            let trimmed = line.trim_start();
            if trimmed.starts_with("#[cfg(test)]") {
                if let Some(next) = lines.peek() {
                    if next.trim_start().starts_with("mod ") {
                        in_test_block = true;
                        depth =
                            next.matches('{').count() as isize - next.matches('}').count() as isize;
                        continue;
                    }
                }
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
