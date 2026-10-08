//! Integration tests for the `dad-local` binary.
//!
//! These drive the real CLI as a subprocess, exactly as a user would: the
//! fast tests cover scaffolding, validation, argument handling, and error
//! reporting; one opt-in test (`--ignored`) runs the full release gate,
//! which compiles a release binary and so is kept out of the default run.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_dad-local");

fn tmp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dad-cli-it-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn run(args: &[&str]) -> Output {
    Command::new(BIN).args(args).output().expect("failed to spawn dad-local")
}

fn run_in(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN).args(args).current_dir(dir).output().expect("failed to spawn dad-local")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn scaffold(dir: &Path, extra: &[&str]) {
    let dir_s = dir.to_str().unwrap();
    let mut args = vec!["init", dir_s];
    args.extend_from_slice(extra);
    let out = run(&args);
    assert!(out.status.success(), "init failed: {}", stderr(&out));
}

#[test]
fn init_writes_the_full_scaffold() {
    let dir = tmp("init");
    scaffold(&dir, &["--id", "org.example.it-addon", "--name", "IT Addon"]);

    for file in [
        "Cargo.toml",
        "manifest.json",
        "src/main.rs",
        "README.md",
        "clippy.toml",
        ".gitignore",
    ] {
        assert!(dir.join(file).exists(), "scaffold is missing {file}");
    }

    let out = run(&["validate", dir.to_str().unwrap()]);
    assert!(out.status.success(), "scaffold must validate: {}", stderr(&out));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn validate_accepts_a_scaffolded_manifest() {
    let dir = tmp("validate-ok");
    scaffold(&dir, &["--id", "org.example.it-validate"]);

    let out = run(&["validate", dir.to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("manifest.json is valid"), "{text}");
    assert!(text.contains("org.example.it-validate"), "{text}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn validate_reports_invalid_json() {
    let dir = tmp("validate-json");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("manifest.json"), "this is not json").unwrap();

    let out = run(&["validate", dir.to_str().unwrap()]);
    assert!(!out.status.success(), "non-JSON must be rejected");
    assert!(stderr(&out).contains("not valid JSON"), "{}", stderr(&out));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn validate_reports_contract_violations() {
    let dir = tmp("validate-contract");
    std::fs::create_dir_all(&dir).unwrap();
    let manifest = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("dad-local-runtime")
            .join("reference/demo_open/manifest.json"),
    )
    .unwrap()
    .replace("org.example.demo-open", "not-reverse-dns");
    std::fs::write(dir.join("manifest.json"), manifest).unwrap();

    let out = run(&["validate", dir.to_str().unwrap()]);
    assert!(!out.status.success(), "contract violation must be rejected");
    let err = stderr(&out);
    assert!(err.contains("violates the DAD local contract"), "{err}");
    assert!(err.contains("reverse-DNS"), "{err}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn validate_reports_a_missing_manifest() {
    let dir = tmp("validate-missing");
    std::fs::create_dir_all(&dir).unwrap();

    let out = run(&["validate", dir.to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("failed to read"), "{}", stderr(&out));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn init_refuses_pre_existing_capabilities_and_dirs() {
    let dir = tmp("init-bad-caps");
    let out = run(&["init", dir.to_str().unwrap(), "--caps", "direct_stream,warp_drive"]);
    assert!(!out.status.success(), "unknown capability must be rejected");
    assert!(stderr(&out).contains("--caps"), "{}", stderr(&out));

    let dir2 = tmp("init-nonempty");
    std::fs::create_dir_all(&dir2).unwrap();
    std::fs::write(dir2.join("occupied.txt"), "x").unwrap();
    let out = run(&["init", dir2.to_str().unwrap(), "--caps", "subtitle"]);
    assert!(!out.status.success(), "a non-empty target dir must be rejected");
    assert!(stderr(&out).contains("not empty"), "{}", stderr(&out));

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&dir2);
}

#[test]
fn help_lists_every_command_and_version_reports() {
    let out = run(&["--help"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    for command in ["init", "validate", "build", "dev", "test"] {
        assert!(text.contains(command), "--help must mention `{command}`:\n{text}");
    }

    let out = run(&["--version"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("dad-local"), "{}", stdout(&out));
}

#[test]
fn verify_current_dir_defaults_to_the_addon() {
    let dir = tmp("cwd-default");
    scaffold(&dir, &["--id", "org.example.it-cwd"]);

    // No positional dir: validate operates on the current directory.
    let out = run_in(&dir, &["validate"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("manifest.json is valid"), "{}", stdout(&out));

    let _ = std::fs::remove_dir_all(&dir);
}

/// The complete release pipeline: scaffold -> `dad-local test` builds a
/// release binary, runs the clippy gate, checks portability, and probes every
/// fixture. Slow (a full release + LTO build), hence opt-in.
#[test]
#[ignore = "compiles a full release binary; run with `cargo test -p dad-local-cli -- --ignored`"]
fn end_to_end_gate_passes_on_a_fresh_scaffold() {
    let dir = tmp("e2e");
    scaffold(&dir, &["--id", "org.example.it-e2e", "--caps", "direct_stream"]);

    let out = run(&["test", dir.to_str().unwrap()]);
    let so = stdout(&out);
    assert!(out.status.success(), "test gate failed:\n{so}\n{}", stderr(&out));
    assert!(so.contains("[OK] clippy clean"), "{so}");
    assert!(so.contains("PASS"), "{so}");

    // The addon binary must actually exist in the scaffold's target dir.
    let name = dir.file_name().unwrap().to_string_lossy().to_string();
    let exe = if cfg!(windows) { format!("{name}.exe") } else { name };
    assert!(
        dir.join("target").join("release").join(exe).exists(),
        "release artifact missing in {}",
        dir.display()
    );

    let _ = std::fs::remove_dir_all(&dir);
}