//! Runs a Nix evaluator. Taken from nixfresh (MIT) and trimmed: there is no
//! sandbox, because the only thing evaluated is a nixpkgs tarball pinned by
//! revision.

use anyhow::{Context, Result, bail};
use std::process::Command;
use std::sync::OnceLock;

const FEATURES: &str = "nix-command flakes";

static EVALUATOR: OnceLock<String> = OnceLock::new();

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
    let dir = std::env::temp_dir().join(format!("maintainer-mode-{kind}-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("input.json"), serde_json::to_vec(input)?)?;
    let path = dir.join(format!("{kind}.nix"));
    std::fs::write(&path, expr)?;
    let text = run_with(
        program,
        &["eval", "--json", "--file", &path.to_string_lossy()],
    );
    // a failed eval leaves the scratch directory behind: the generated
    // expression is the only way to read the nix error that names it.
    if text.is_ok() {
        let _ = std::fs::remove_dir_all(&dir);
    }
    text
}
