//! Glob matcher (`**`, `*`, `?`) and mtime-sorted directory walking.

fn match_segment(pat: &[char], s: &[char]) -> bool {
    // classic iterative wildcard match for * and ?
    let (mut p, mut i) = (0usize, 0usize);
    let (mut star, mut mark) = (usize::MAX, 0usize);
    while i < s.len() {
        if p < pat.len() && (pat[p] == '?' || pat[p] == s[i]) {
            p += 1;
            i += 1;
        } else if p < pat.len() && pat[p] == '*' {
            star = p;
            mark = i;
            p += 1;
        } else if star != usize::MAX {
            p = star + 1;
            mark += 1;
            i = mark;
        } else {
            return false;
        }
    }
    while p < pat.len() && pat[p] == '*' {
        p += 1;
    }
    p == pat.len()
}

/// Match `path` against `pattern` with `/`-aware `**`.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    let pat_parts: Vec<&str> = pattern.split('/').collect();
    let path_parts: Vec<&str> = path.split('/').collect();
    match_parts(&pat_parts, &path_parts)
}

fn match_parts(pat: &[&str], path: &[&str]) -> bool {
    if pat.is_empty() {
        return path.is_empty();
    }
    if pat[0] == "**" {
        // `**` matches zero or more path segments
        for skip in 0..=path.len() {
            if match_parts(&pat[1..], &path[skip..]) {
                return true;
            }
        }
        return false;
    }
    if path.is_empty() {
        return false;
    }
    let pc: Vec<char> = pat[0].chars().collect();
    let sc: Vec<char> = path[0].chars().collect();
    if !match_segment(&pc, &sc) {
        return false;
    }
    match_parts(&pat[1..], &path[1..])
}

/// Walk `root` collecting files matching `pattern` (relative paths),
/// sorted by mtime descending. Respects a simple ignore set.
pub fn walk_glob(root: &std::path::Path, pattern: &str, limit: usize) -> Vec<std::path::PathBuf> {
    let mut hits: Vec<(std::time::SystemTime, std::path::PathBuf)> = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || name == "node_modules" || name == "target" {
                continue;
            }
            if p.is_dir() {
                stack.push(p);
            } else {
                let rel = p
                    .strip_prefix(root)
                    .unwrap_or(&p)
                    .to_string_lossy()
                    .to_string();
                if glob_match(pattern, &rel) {
                    let mtime = e
                        .metadata()
                        .and_then(|m| m.modified())
                        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                    hits.push((mtime, p));
                }
            }
        }
    }
    hits.sort_by_key(|(mtime, _)| std::cmp::Reverse(*mtime));
    hits.into_iter().take(limit).map(|(_, p)| p).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_wildcards() {
        assert!(match_segment(&['*'], &['a', 'b', 'c']));
        assert!(match_segment(&['a', '?', 'c'], &['a', 'b', 'c']));
        assert!(!match_segment(&['a', '?'], &['a', 'b', 'c']));
        assert!(match_segment(
            &['*', '.', 'r', 's'],
            &['m', 'a', 'i', 'n', '.', 'r', 's']
        ));
    }

    #[test]
    fn path_globs() {
        assert!(glob_match("*.rs", "main.rs"));
        assert!(!glob_match("*.rs", "src/main.rs"));
        assert!(glob_match("**/*.rs", "src/deep/main.rs"));
        assert!(glob_match("src/*.rs", "src/main.rs"));
        assert!(!glob_match("src/*.rs", "src/deep/main.rs"));
    }

    #[test]
    fn walk_glob_finds_and_sorts() {
        let root = marfim_testkit::unique_dir("marfim-glob");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src/deep")).unwrap();
        std::fs::write(root.join("a.rs"), "1").unwrap();
        std::fs::write(root.join("src/b.rs"), "2").unwrap();
        std::fs::write(root.join("src/deep/c.rs"), "3").unwrap();
        std::fs::write(root.join("skip.txt"), "x").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(root.join("src/b.rs"), "22").unwrap();
        let hits = walk_glob(&root, "**/*.rs", 10);
        assert_eq!(hits.len(), 3);
        assert!(hits.iter().any(|p| p.ends_with("a.rs")));
        // hidden files skipped
        std::fs::write(root.join(".hidden.rs"), "x").unwrap();
        let hits2 = walk_glob(&root, "**/*.rs", 10);
        assert_eq!(hits2.len(), 3);
    }
}
