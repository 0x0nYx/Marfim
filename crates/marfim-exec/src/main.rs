//! marfim-exec — the standalone sandbox exec-helper (W1.4).
//!
//! One tiny binary, depending only on marfim-core, that packages can
//! ship exec-able on read-only mounts (Android `nativeLibraryDir`):
//!
//! ```text
//! marfim-exec [--root R]... -- <argv...>
//! ```
//!
//! Applies Landlock write-roots (Linux) / refuses when the platform
//! cannot enforce, then execs the target — fail-closed BEFORE exec, so
//! an enforcement error means the child never runs. Exit codes:
//! 0 = child exited 0; otherwise the child's own code; 2 usage;
//! 84 sandbox refusal (conventional); 127 exec failure.

#![forbid(unsafe_code)]

fn main() {
    std::process::exit(run());
}

fn run() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut roots: Vec<String> = Vec::new();
    let mut cmd: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--root" if i + 1 < args.len() => {
                roots.push(args[i + 1].clone());
                i += 2;
            }
            "--" => {
                cmd = args[i + 1..].to_vec();
                break;
            }
            other => {
                eprintln!("error: marfim-exec: unexpected argument `{other}`");
                eprintln!("usage: marfim-exec [--root DIR]... -- <argv...>");
                return 2;
            }
        }
    }
    if cmd.is_empty() {
        eprintln!("error: marfim-exec needs a command after `--`");
        return 2;
    }
    marfim_core::sandbox::exec_enforced(&roots, &cmd)
}
