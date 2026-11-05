//! Argv-level shell command analysis (INV-08): tokenize a command line,
//! split on `;` `&&` `||` `|` (quote-aware), detect heredocs, `$()`,
//! backticks, process substitution, redirects. Each segment is checked
//! independently; anything containing nested execution constructs can
//! NEVER be auto-allowed past `ask`.

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ShellRisk {
    /// Clean, single command, no nested execution / redirection.
    Simple,
    /// Redirects present (`>`, `>>`, `<`, `2>`): may write outside intent.
    Redirect,
    /// Pipelines: every stage must be checked.
    Pipeline,
    /// Sequenced commands (`;`, `&&`, `||`): every segment checked.
    Sequenced,
    /// Heredoc (`<<`, `<<-`, `<<<`): body is opaque; never auto-allow.
    Heredoc,
    /// Nested execution (`$(...)`, backticks, process substitution
    /// `<( )`): never auto-allow.
    NestedExec,
}

#[derive(Debug)]
pub struct Analysis {
    pub risk: ShellRisk,
    /// Per-segment argv (after splitting on operators). Redirect targets
    /// remain in the segment argv so rules can inspect them.
    pub segments: Vec<Vec<String>>,
    pub redirects: Vec<String>,
    pub heredoc_bodies: Vec<String>,
}

/// Tokenize honoring single/double quotes and backslash escapes (argv
/// semantics; quotes removed).
fn tokenize(cmd: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = cmd.chars().peekable();
    let mut started = false;
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\n' => {
                if started {
                    out.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            '\'' => {
                started = true;
                for c2 in chars.by_ref() {
                    if c2 == '\'' {
                        break;
                    }
                    cur.push(c2);
                }
            }
            '"' => {
                started = true;
                while let Some(c2) = chars.next() {
                    if c2 == '"' {
                        break;
                    }
                    if c2 == '\\' {
                        if let Some(c3) = chars.next() {
                            cur.push(c3);
                        }
                    } else {
                        cur.push(c2);
                    }
                }
            }
            '\\' => {
                started = true;
                if let Some(c2) = chars.next() {
                    cur.push(c2);
                }
            }
            _ => {
                started = true;
                cur.push(c);
            }
        }
    }
    if started {
        out.push(cur);
    }
    out
}

/// One quote-aware pass over the raw command: extracts redirects (outside
/// quotes), splits on `;` `&&` `||` `|` (outside quotes), flags operators.
struct Scan {
    cleaned: String,
    redirects: Vec<String>,
    has_pipe: bool,
    has_seq: bool,
    heredoc: bool,
}

fn scan_operators(cmd: &str) -> Scan {
    let chars: Vec<char> = cmd.chars().collect();
    let mut cleaned = String::new();
    let mut redirects = Vec::new();
    let mut has_pipe = false;
    let mut has_seq = false;
    let mut heredoc = false;
    let mut in_single = false;
    let mut in_double = false;
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        // backslash escape outside single quotes
        if c == '\\' && !in_single && i + 1 < chars.len() {
            cleaned.push(c);
            cleaned.push(chars[i + 1]);
            i += 2;
            continue;
        }
        if c == '\'' && !in_double {
            in_single = !in_single;
            cleaned.push(c);
            i += 1;
            continue;
        }
        if c == '"' && !in_single {
            in_double = !in_double;
            cleaned.push(c);
            i += 1;
            continue;
        }
        if in_single || in_double {
            cleaned.push(c);
            i += 1;
            continue;
        }
        // operators outside quotes
        match c {
            '>' => {
                if i + 1 < chars.len() && chars[i + 1] == '>' {
                    redirects.push(">>".into());
                    i += 2;
                } else if i + 1 < chars.len() && chars[i + 1] == '&' {
                    redirects.push(">&".into());
                    i += 2;
                } else {
                    redirects.push(">".into());
                    i += 1;
                }
                cleaned.push(' '); // target stays in the same segment
            }
            '<' => {
                if i + 1 < chars.len() && chars[i + 1] == '<' {
                    heredoc = true;
                    if i + 2 < chars.len() && chars[i + 2] == '<' {
                        i += 3; // herestring <<<
                    } else {
                        i += 2;
                    }
                    cleaned.push(' ');
                } else {
                    redirects.push("<".into());
                    i += 1;
                    cleaned.push(' ');
                }
            }
            ';' => {
                has_seq = true;
                i += 1;
                cleaned.push('\n'); // hard split point
            }
            '|' => {
                if i + 1 < chars.len() && chars[i + 1] == '|' {
                    has_seq = true;
                    i += 2;
                } else {
                    has_pipe = true;
                    i += 1;
                }
                cleaned.push('\n'); // hard split point
            }
            '&' => {
                if i + 1 < chars.len() && chars[i + 1] == '&' {
                    has_seq = true;
                    i += 2;
                    cleaned.push('\n');
                } else {
                    cleaned.push(c); // background & stays visible in argv
                    i += 1;
                }
            }
            '2' if i + 1 < chars.len() && chars[i + 1] == '>' => {
                if i + 2 < chars.len() && chars[i + 2] == '>' {
                    redirects.push("2>>".into());
                    i += 3;
                } else {
                    redirects.push("2>".into());
                    i += 2;
                }
                cleaned.push(' '); // target stays in the same segment
            }
            _ => {
                cleaned.push(c);
                i += 1;
            }
        }
    }
    Scan {
        cleaned,
        redirects,
        has_pipe,
        has_seq,
        heredoc,
    }
}

/// Extract heredoc/herestring bodies (terminator-delimited) from the raw
/// command, outside quotes.
fn extract_heredoc_bodies(cmd: &str) -> Vec<String> {
    let mut bodies = Vec::new();
    let chars: Vec<char> = cmd.chars().collect();
    let mut in_single = false;
    let mut in_double = false;
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && !in_single && i + 1 < chars.len() {
            i += 2;
            continue;
        }
        if c == '\'' && !in_double {
            in_single = !in_single;
            i += 1;
            continue;
        }
        if c == '"' && !in_single {
            in_double = !in_double;
            i += 1;
            continue;
        }
        if !in_single && !in_double && c == '<' && i + 1 < chars.len() && chars[i + 1] == '<' {
            let mut j = i + 2;
            let herestring = j < chars.len() && chars[j] == '<';
            if herestring {
                j += 1;
            }
            // skip leading whitespace, read terminator token
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            let start = j;
            while j < chars.len() && !chars[j].is_whitespace() {
                j += 1;
            }
            let term: String = chars[start..j].iter().collect();
            i = j;
            if herestring || term.is_empty() {
                continue;
            }
            // body = lines after terminator token until a line == term
            let rest: String = chars[j..].iter().collect();
            let after = rest.trim_start_matches(['\r', '\n']);
            let mut body = String::new();
            for line in after.lines() {
                if line.trim_end() == term {
                    break;
                }
                body.push_str(line);
                body.push('\n');
            }
            bodies.push(body);
            continue;
        }
        i += 1;
    }
    bodies
}

/// Full analysis. `cmd` is a raw shell string (as produced by the model).
pub fn analyze(cmd: &str) -> Analysis {
    let mut risk = ShellRisk::Simple;

    let heredoc_bodies = extract_heredoc_bodies(cmd);

    // nested execution anywhere (also inside double quotes, where it
    // still executes)
    let nested =
        cmd.contains("$(") || cmd.contains('`') || cmd.contains("<(") || cmd.contains(">(");
    if nested {
        risk = risk.max(ShellRisk::NestedExec);
    }

    let s = scan_operators(cmd);
    if s.heredoc || !heredoc_bodies.is_empty() {
        risk = risk.max(ShellRisk::Heredoc);
    }
    if !s.redirects.is_empty() {
        risk = risk.max(ShellRisk::Redirect);
    }
    if s.has_pipe {
        risk = risk.max(ShellRisk::Pipeline);
    }
    if s.has_seq {
        risk = risk.max(ShellRisk::Sequenced);
    }

    let segments: Vec<Vec<String>> = s
        .cleaned
        .split('\n')
        .map(str::trim)
        .filter(|seg| !seg.is_empty())
        .flat_map(|seg| {
            let v = tokenize(seg);
            if v.is_empty() {
                vec![]
            } else {
                vec![v]
            }
        })
        .collect();

    Analysis {
        risk,
        segments,
        redirects: s.redirects,
        heredoc_bodies,
    }
}

// derive(Ord) orders variants by declaration: Simple < Redirect < Pipeline
// < Sequenced < Heredoc < NestedExec — exactly the risk escalation order.

#[cfg(test)]
mod tests {
    use super::*;

    fn seg0(cmd: &str) -> Vec<String> {
        analyze(cmd).segments[0].clone()
    }

    #[test]
    fn simple_command_tokenizes() {
        assert_eq!(seg0("ls -la /tmp"), vec!["ls", "-la", "/tmp"]);
        assert_eq!(seg0("echo 'a b' \"c d\""), vec!["echo", "a b", "c d"]);
        assert_eq!(seg0("grep \"it's\" f"), vec!["grep", "it's", "f"]);
    }

    #[test]
    fn heredoc_never_simple() {
        let a = analyze("bash <<EOF\necho hi\nEOF");
        assert_eq!(a.risk, ShellRisk::Heredoc);
        assert!(a.heredoc_bodies[0].contains("echo hi"));
    }

    #[test]
    fn nested_exec_detected() {
        assert_eq!(analyze("echo $(whoami)").risk, ShellRisk::NestedExec);
        assert_eq!(analyze("echo `id`").risk, ShellRisk::NestedExec);
        assert_eq!(analyze("cat <(ls)").risk, ShellRisk::NestedExec);
        assert_eq!(analyze("diff <(ls) <(ls -a)").risk, ShellRisk::NestedExec);
    }

    #[test]
    fn redirects_detected_with_targets_split() {
        let a = analyze("echo hi > /etc/passwd");
        assert_eq!(a.risk, ShellRisk::Redirect);
        assert_eq!(a.redirects, vec![">"]);
        assert_eq!(a.segments[0], vec!["echo", "hi", "/etc/passwd"]);
    }

    #[test]
    fn sequencing_split_and_flagged() {
        let a = analyze("rm -rf a && echo done; cat b || true");
        assert_eq!(a.risk, ShellRisk::Sequenced);
        // 4 segments: rm…, echo…, cat b, true (|| is also a split point)
        assert_eq!(a.segments.len(), 4);
        assert_eq!(a.segments[0], vec!["rm", "-rf", "a"]);
        assert_eq!(a.segments[1], vec!["echo", "done"]);
        assert_eq!(a.segments[2], vec!["cat", "b"]);
        assert_eq!(a.segments[3], vec!["true"]);
    }

    #[test]
    fn pipeline_detected() {
        let a = analyze("cat f | grep x | wc -l");
        assert_eq!(a.risk, ShellRisk::Pipeline);
        assert_eq!(a.segments.len(), 3);
        assert_eq!(a.segments[0], vec!["cat", "f"]);
        assert_eq!(a.segments[1], vec!["grep", "x"]);
        assert_eq!(a.segments[2], vec!["wc", "-l"]);
    }

    #[test]
    fn quoted_operators_do_not_split() {
        let a = analyze("echo 'a; b' && echo c");
        assert_eq!(a.segments[0], vec!["echo", "a; b"]);
        assert_eq!(a.segments[1], vec!["echo", "c"]);
        let r = analyze("grep \"a|b\" file");
        assert_eq!(r.segments[0], vec!["grep", "a|b", "file"]);
        assert_eq!(r.risk, ShellRisk::Simple);
    }

    #[test]
    fn stderr_redirect_detected() {
        let a = analyze("ls 2>/dev/null");
        assert_eq!(a.redirects, vec!["2>"]);
        assert_eq!(a.segments[0], vec!["ls", "/dev/null"]);
    }

    #[test]
    fn herestring_is_opaque() {
        let a = analyze("tr a b <<< \"text\"");
        assert_eq!(a.risk, ShellRisk::Heredoc);
    }

    #[test]
    fn quoted_redirect_not_detected() {
        let a = analyze("echo \"a > b\"");
        assert_eq!(a.risk, ShellRisk::Simple);
        assert!(a.redirects.is_empty());
    }
}
