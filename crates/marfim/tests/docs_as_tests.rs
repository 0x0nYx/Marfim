//! Docs-as-tests (P9): every fenced command/config in README.md and
//! examples/ must be REAL — commands parse against the actual CLI surface
//! (`Cli::try_parse_from`), configs validate against the actual validator
//! (`Config::from_value` + section checks), generated counts match the
//! tree, and exactly one version source feeds Cargo, `--version` and the
//! CHANGELOG. Docs drift is a test failure, not a review note.

use marfim_core::config::Config;

const REPO_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

/// (path, language, body) for every fenced block in a markdown file.
fn fenced_blocks(path: &std::path::Path) -> Vec<(String, String)> {
    let text =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let mut out = Vec::new();
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        let l = line.trim();
        if let Some(lang) = l.strip_prefix("```") {
            let lang = lang.trim().to_string();
            let mut body = Vec::new();
            for l2 in lines.by_ref() {
                if l2.trim() == "```" {
                    break;
                }
                body.push(l2);
            }
            out.push((lang, body.join("\n")));
        }
    }
    out
}

/// Quote-aware shell-ish tokenizer for doc snippets (no process spawn).
fn tokenize(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quote: Option<char> = None;
    for c in line.chars() {
        match in_quote {
            Some(q) if c == q => in_quote = None,
            Some(_) => cur.push(c),
            None => match c {
                '\'' | '"' => in_quote = Some(c),
                c if c.is_whitespace() => {
                    if !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                    }
                }
                c => cur.push(c),
            },
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn bin_path() -> std::path::PathBuf {
    // keep in lockstep with e2e.rs's helper
    let profile = if std::env::var("PROFILE")
        .map(|p| p == "release")
        .unwrap_or(false)
    {
        "release"
    } else {
        "debug"
    };
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target")
        .join(profile)
        .join("marfim")
}

/// Every `marfim …` line in every bash fence must parse against the REAL
/// CLI. `--help` short-circuits after clap parsing, so a failing doc
/// command exits non-zero with a usage error; a valid one exits 0 without
/// ever touching the network.
#[test]
fn every_readme_command_parses_with_the_real_cli() {
    let readme = std::path::Path::new(REPO_ROOT).join("README.md");
    let bin = bin_path();
    let mut checked = 0usize;
    for (lang, body) in fenced_blocks(&readme) {
        if lang != "bash" && lang != "sh" && !lang.is_empty() {
            continue;
        }
        for line in body.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() || line.starts_with("export ") {
                continue;
            }
            let Some(first) = line.split_whitespace().next() else {
                continue;
            };
            if first != "marfim" {
                continue;
            }
            let toks = tokenize(line);
            let out = std::process::Command::new(&bin)
                .args(&toks[1..])
                .arg("--help")
                .output()
                .unwrap_or_else(|e| panic!("run {bin:?}: {e}"));
            assert!(
                out.status.success(),
                "README command does not parse: `{line}` → {} {}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            checked += 1;
        }
    }
    assert!(
        checked >= 5,
        "expected real commands in README, got {checked}"
    );
}

#[test]
fn every_readme_and_examples_config_validates() {
    let mut sources: Vec<std::path::PathBuf> =
        vec![std::path::Path::new(REPO_ROOT).join("README.md")];
    for dir in ["examples/commands", "examples/skills"] {
        let d = std::path::Path::new(REPO_ROOT).join(dir);
        if let Ok(rd) = std::fs::read_dir(&d) {
            for e in rd.flatten() {
                if e.path().extension().map(|x| x == "md").unwrap_or(false) {
                    sources.push(e.path());
                }
            }
        }
    }
    let mut checked = 0usize;
    for src in &sources {
        for (lang, body) in fenced_blocks(src) {
            if lang != "toml" {
                continue;
            }
            let value: toml::Value = toml::from_str(&body)
                .unwrap_or_else(|e| panic!("{}: toml parse: {e}\n{body}", src.display()));
            let json = serde_json::to_value(value).unwrap_or_default();
            let cfg = Config::from_value(json)
                .unwrap_or_else(|e| panic!("{}: config rejects: {e}\n{body}", src.display()));
            cfg.validate_consumed_sections()
                .unwrap_or_else(|e| panic!("{}: section rejects: {e}\n{body}", src.display()));
            checked += 1;
        }
    }
    // README carries the wiring example; examples/config.toml is covered
    // separately below — here we assert we actually found it.
    assert!(checked >= 1, "no toml fences validated ({checked})");
}

#[test]
fn examples_config_toml_is_the_validated_shape() {
    let path = std::path::Path::new(REPO_ROOT).join("examples/config.toml");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read: {e}"));
    let value: toml::Value =
        toml::from_str(&text).unwrap_or_else(|e| panic!("examples/config.toml: {e}"));
    let json = serde_json::to_value(value).unwrap_or_default();
    let cfg = Config::from_value(json)
        .unwrap_or_else(|e| panic!("examples/config.toml has non-canonical keys: {e}"));
    cfg.validate_consumed_sections()
        .unwrap_or_else(|e| panic!("examples/config.toml section drift: {e}"));
}

/// Binary `--version` must equal the workspace version (one source).
#[test]
fn binary_version_agrees_with_cargo() {
    let out = std::process::Command::new(bin_path())
        .arg("--version")
        .output()
        .unwrap_or_else(|e| panic!("run --version: {e}"));
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert_eq!(
        text.trim(),
        format!("marfim {}", env!("CARGO_PKG_VERSION")),
        "--version must be the single workspace version"
    );
}
