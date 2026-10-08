//! Driving `cargo` through this machine's real build environment.
//!
//! An addon is built by spawning `cargo`, but the environment that build needs
//! — MSVC's `PATH`/`LIB`/`INCLUDE`, and its own `link.exe` — is not guaranteed
//! to be present in the shell that started us. This crate is the single place
//! that resolves that environment, so the CLI and the SDK's own compile-contract
//! tests drive cargo identically and cannot drift apart.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// Cached `vcvarsall.bat` lookup — resolved once per process instead of
/// re-shelling `vswhere.exe` on every cargo invocation.
static VCVARSALL: OnceLock<Option<PathBuf>> = OnceLock::new();

/// The Microsoft-supported build environment: `vcvarsall.bat`, located with
/// `vswhere.exe` — which is what vswhere exists for. It hands back `PATH`,
/// `LIB` and `INCLUDE` for THIS machine's toolchain, maintained by Microsoft,
/// instead of re-deriving them here from the registry and a version sort
/// (that is vcvarsall's private logic and it drifts).
///
/// This is also what makes Git Bash work: Git for Windows ships its own
/// `/usr/bin/link`, which otherwise shadows the real `link.exe`. Running
/// cargo inside the vcvars environment is immune to shell PATH ordering.
///
/// Returns `None` on non-Windows hosts, and on Windows when Visual Studio /
/// Build Tools cannot be found — the caller then runs plain `cargo` and lets
/// rustc's own auto-detection speak for itself.
pub fn find_vcvarsall() -> Option<PathBuf> {
    VCVARSALL.get_or_init(discover_vcvarsall).clone()
}

fn discover_vcvarsall() -> Option<PathBuf> {
    if !cfg!(windows) {
        return None;
    }
    // Microsoft's canonical vswhere.exe location, resolved from the OS
    // environment so a non-standard system root still works. No literal
    // path is written into this crate.
    let program_files =
        std::env::var_os("ProgramFiles(x86)").or_else(|| std::env::var_os("ProgramFiles"))?;
    let vswhere = PathBuf::from(program_files)
        .join("Microsoft Visual Studio")
        .join("Installer")
        .join("vswhere.exe");
    let output = Command::new(&vswhere)
        .args([
            "-latest",
            "-products",
            "*",
            "-requires",
            "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
            "-property",
            "installationPath",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let install_path = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if install_path.is_empty() {
        return None;
    }
    let vcvars = PathBuf::from(&install_path)
        .join("VC")
        .join("Auxiliary")
        .join("Build")
        .join("vcvarsall.bat");
    vcvars.is_file().then_some(vcvars)
}

/// Host architecture argument for `vcvarsall.bat <arch>` — the host tools
/// profile (x64 host tools, arm64 host tools, ...).
fn vcvars_host_arch() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "arm64"
    } else if cfg!(target_arch = "x86") {
        "x86"
    } else {
        "x64"
    }
}

/// Builds the cargo invocation for this machine.
///
/// With Visual Studio installed this goes through `cmd /C call vcvarsall.bat
/// <arch> >nul && cargo <args>`, so the toolchain environment is the one
/// Microsoft's own script configures — from PowerShell, cmd, or Git Bash
/// alike, with no environment variables of ours written anywhere.
///
/// Without it, cargo runs directly: rustc auto-detects the MSVC toolchain
/// itself (it does so on any normal PowerShell/cmd shell), and if there is no
/// toolchain at all cargo reports its own error.
///
/// The returned `Command` is unconfigured beyond that: callers set the working
/// directory and any extra environment, then choose `.status()` or `.output()`.
pub fn cargo_command(args: &[&str]) -> Command {
    if let Some(vcvars) = find_vcvarsall() {
        // Tokens are passed individually; only vcvarsall's path is quoted, by
        // the OS argument builder. `call` is required - without it cmd would
        // hand control to the batch file and never run the `&& cargo` half.
        let mut command = Command::new("cmd");
        command
            .arg("/C")
            .arg("call")
            .arg(&vcvars)
            .arg(vcvars_host_arch())
            .arg(">nul")
            .arg("&&")
            .arg("cargo")
            .args(args);
        command
    } else {
        let mut command = Command::new("cargo");
        command.args(args);
        command
    }
}

/// Runs `cargo <args>` inside `dir`, streaming its output to this process's
/// own stdout/stderr.
pub fn cargo(dir: &Path, args: &[&str]) -> Result<(), String> {
    let status = cargo_command(args)
        .current_dir(dir)
        .status()
        .map_err(|e| format!("failed to launch cargo (is it on PATH?): {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`cargo {}` failed with {status}", args.join(" ")))
    }
}

/// Runs `cargo <args>` inside `dir` with extra environment `env`, capturing
/// stdout+stderr instead of streaming it — for gates and tests where the
/// caller decides whether the output is worth showing.
pub fn cargo_capture(
    dir: &Path,
    args: &[&str],
    env: &[(&str, &OsStr)],
) -> Result<(bool, String), String> {
    let mut command = cargo_command(args);
    command.current_dir(dir);
    for (key, value) in env {
        command.env(key, value);
    }
    let output = command
        .output()
        .map_err(|e| format!("failed to launch cargo (is it on PATH?): {e}"))?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok((output.status.success(), text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_hardcoded_paths_or_toolchain_versions_in_this_module() {
        // The whole point of delegating to vswhere + vcvarsall: this crate must
        // not bake in a machine's Program Files root or a toolchain version.
        // Only the production half is scanned - this test names the banned
        // tokens, so it must not be its own evidence.
        let source = include_str!("lib.rs");
        let production = source.split("#[cfg(test)]").next().unwrap_or(source);
        for banned in [
            "Program Files",
            "Windows Kits",
            "Installed Roots",
            "14.44",
            "26100",
            "Sparsho",
        ] {
            assert!(
                !production.contains(banned),
                "hardcoded '{banned}' must not creep back into the toolchain crate"
            );
        }
    }

    #[test]
    fn vcvarsall_resolves_to_a_real_batch_file_when_vs_is_installed() {
        // Skips itself (returns None) on non-Windows and on boxes without
        // Visual Studio - that is the documented fallback, not a failure.
        if let Some(vcvars) = find_vcvarsall() {
            assert!(vcvars.is_file(), "vcvarsall must exist: {}", vcvars.display());
            assert_eq!(
                vcvars.file_name().and_then(|n| n.to_str()),
                Some("vcvarsall.bat"),
                "unexpected script name: {}",
                vcvars.display()
            );
            let text = vcvars.to_string_lossy();
            assert!(text.contains("Auxiliary"), "expected the standard VS layout: {text}");
        }
    }
}