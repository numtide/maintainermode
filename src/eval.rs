//! Runs a Nix evaluator. Taken from nixfresh (MIT) and trimmed: there is no
//! sandbox, because the only thing evaluated is a nixpkgs tarball pinned by
//! revision.

use anyhow::{Context, Result, bail};
use std::process::Command;
use std::sync::OnceLock;

const FEATURES: &str = "nix-command flakes";

static EVALUATOR: OnceLock<String> = OnceLock::new();

/// One nixpkgs sweep at a time: each one peaks near 2 GB.
pub static SWEEP: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The one knob that picks the evaluator: iets in a deployment, plain nix
/// anywhere else. Anything that answers `nix eval --json --file` works, so the
/// two can be swapped and timed against each other.
pub fn set_evaluator(program: &str) {
    let _ = EVALUATOR.set(program.to_string());
}

pub fn evaluator() -> &'static str {
    EVALUATOR.get().map(String::as_str).unwrap_or("nix")
}

pub fn run_with(program: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(program)
        .args(["--extra-experimental-features", FEATURES])
        .args(args)
        .output()
        .with_context(|| format!("run {program}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail: Vec<&str> = err.lines().rev().take(15).collect();
        bail!(
            "{program} {} failed: {}",
            args.join(" "),
            tail.into_iter().rev().collect::<Vec<_>>().join("\n")
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Evaluates a generated expression against a JSON input file. The input goes in
/// a file rather than the command line: names and paths cannot be escaped into a
/// nix expression safely.
pub fn eval_with_input(kind: &str, expr: &str, input: &serde_json::Value) -> Result<String> {
    eval_with_input_using(evaluator(), kind, expr, input)
}

pub fn eval_with_input_using(
    program: &str,
    kind: &str,
    expr: &str,
    input: &serde_json::Value,
) -> Result<String> {
    let dir = private_dir(kind)?;
    let input_path = dir.join("input.json");
    std::fs::write(&input_path, serde_json::to_vec(input)?)?;
    let path = dir.join(format!("{kind}.nix"));
    std::fs::write(&path, expr)?;
    let text = run_with(
        program,
        &["eval", "--json", "--file", &path.to_string_lossy()],
    );
    // a failed eval keeps the generated expression, the only way to read the
    // nix error that names it; the input can hold private package names.
    if text.is_ok() {
        let _ = std::fs::remove_dir_all(&dir);
    } else {
        let _ = std::fs::remove_file(&input_path);
    }
    text
}

/// A new directory only the owner can enter. It refuses a path that exists, so
/// another local user cannot plant the expression that runs.
fn private_dir(kind: &str) -> Result<std::path::PathBuf> {
    use rand::Rng;
    use std::os::unix::fs::DirBuilderExt;
    for _ in 0..16 {
        let dir = std::env::temp_dir().join(format!(
            "maintainer-mode-{kind}-{:016x}",
            rand::thread_rng().gen::<u64>()
        ));
        match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e).with_context(|| format!("create {}", dir.display())),
        }
    }
    bail!(
        "no free scratch directory in {}",
        std::env::temp_dir().display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn the_scratch_directory_is_new_and_private() {
        let a = private_dir("test").unwrap();
        let b = private_dir("test").unwrap();
        assert_ne!(a, b);
        let mode = std::fs::metadata(&a).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        std::fs::remove_dir(a).unwrap();
        std::fs::remove_dir(b).unwrap();
    }

    #[test]
    fn a_failed_eval_drops_the_input() {
        let marker = format!("private-{}", std::process::id());
        let r = eval_with_input_using("false", "failtest", "{}", &serde_json::json!([marker]));
        assert!(r.is_err());
        let left: Vec<_> = std::fs::read_dir(std::env::temp_dir())
            .unwrap()
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("maintainer-mode-failtest-")
            })
            .collect();
        for e in &left {
            assert!(!e.path().join("input.json").exists());
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}
