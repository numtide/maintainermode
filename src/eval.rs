//! Runs a Nix evaluator. There is no sandbox, because the only thing
//! evaluated is a nixpkgs tarball pinned by revision.

use anyhow::{Context, Result, bail};
use std::io::Read;
use std::process::{Command, Stdio};
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
    let mut all = vec!["--extra-experimental-features", FEATURES];
    all.extend_from_slice(args);
    run_plain(program, &all)
}

/// For a native iets command, which takes no Nix flag before its name.
pub fn run_plain(program: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("run {program}"))?;
    if !out.status.success() {
        bail!(failure(
            program,
            args,
            &String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Hands the standard output of the program to `read`, as it comes. The
/// closure of a NixOS system is hundreds of megabytes of JSON, which a reader
/// that parses as it goes never has to hold.
pub fn stream_with<T>(
    program: &str,
    args: &[&str],
    read: impl FnOnce(&mut dyn Read) -> Result<T>,
) -> Result<T> {
    let mut all = vec!["--extra-experimental-features", FEATURES];
    all.extend_from_slice(args);
    let mut child = Command::new(program)
        .args(&all)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("run {program}"))?;
    // the error goes to its own thread: a program that fills the error pipe
    // while nothing reads it would wait there forever.
    let mut err_pipe = child.stderr.take().expect("stderr is a pipe");
    let errors = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = err_pipe.read_to_string(&mut text);
        text
    });
    let mut out = std::io::BufReader::new(child.stdout.take().expect("stdout is a pipe"));
    let value = read(&mut out);
    // a reader that stopped early leaves the rest in the pipe, and the program
    // waits on it; the handle closes on drop, so the wait below ends.
    drop(out);
    let status = child.wait().with_context(|| format!("run {program}"))?;
    let err = errors.join().unwrap_or_default();
    if !status.success() {
        bail!(failure(program, &all, &err));
    }
    value
}

/// The last lines of the error output, which hold the message of nix.
fn failure(program: &str, args: &[&str], err: &str) -> String {
    let tail: Vec<&str> = err.lines().rev().take(15).collect();
    format!(
        "{program} {} failed: {}",
        args.join(" "),
        tail.into_iter().rev().collect::<Vec<_>>().join("\n")
    )
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
pub(crate) fn private_dir(kind: &str) -> Result<std::path::PathBuf> {
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
    fn a_stream_reads_the_output_and_a_failure_tells_the_error() {
        let text = stream_with("echo", &["streamed"], |out| {
            let mut text = String::new();
            out.read_to_string(&mut text)?;
            Ok(text)
        })
        .unwrap();
        assert!(text.contains("streamed"), "{text}");
        let e = stream_with("false", &["x"], |_| Ok(()))
            .unwrap_err()
            .to_string();
        assert!(e.contains("false"), "{e}");
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
