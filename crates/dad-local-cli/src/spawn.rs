//! Cargo driving + one-shot subprocess RPC — the CLI acts as a minimal host.
//!
//! The build-environment half (MSVC discovery, the `vcvarsall`-wrapped cargo
//! invocation) lives in `dad-local-toolchain` so the CLI and the SDK's own
//! compile-contract tests share one implementation.

use dad_local_core::DadTestFixture;
pub use dad_local_toolchain::cargo;
use dad_local_toolchain::cargo_capture;
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The clippy gate's flags.
///
/// `-D warnings` makes every finding clippy raises by default fatal.
/// `-D clippy::disallowed_methods` promotes the scaffold's `clippy.toml`
/// denylist to an error as well — that lint is allow-by-default, so the deny
/// has to come from the command line.
pub fn clippy_args() -> &'static [&'static str] {
    &[
        "clippy",
        "--quiet",
        "--",
        "-D",
        "warnings",
        "-D",
        "clippy::disallowed_methods",
    ]
}

/// `cargo clippy` as a hard gate, using [`clippy_args`].
///
/// Only the ADDON crate is linted: the scaffold is its own workspace, so
/// `dad-local-core` and `dad-local-runtime` are plain path dependencies and
/// never reach clippy-driver. The gate cannot fail on SDK-owned code.
///
/// Output is held back on success and printed on failure, so a clean run
/// costs one `[OK]` line.
pub fn clippy_gate(dir: &Path) -> Result<(), String> {
    let (ok, text) = cargo_capture(dir, clippy_args(), &[])?;
    if ok {
        println!(" [OK] clippy clean");
        return Ok(());
    }
    eprint!("{text}");
    if text.contains("no such command") {
        return Err(
            "clippy is not installed for this toolchain\n  install it with \
             `rustup component add clippy`, or pass --skip-clippy to run without the gate"
                .to_string(),
        );
    }
    Err(
        "clippy found problems in the addon\n  fix the findings above, or pass \
         --skip-clippy to run without the gate"
            .to_string(),
    )
}

/// `cargo check --target <triple>` — the portability gate (no linking, so
/// cross-target checks are cheap where the target's std is installed).
pub fn check_target(dir: &Path, triple: &str) -> Result<(), String> {
    cargo(dir, &["check", "--quiet", "--target", triple])
}

/// Resolves where the built binary lands. Honors `CARGO_TARGET_DIR`.
pub fn artifact_path(addon_dir: &Path, profile: &str, bin_name: &str) -> PathBuf {
    let target_root = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| addon_dir.join("target"));
    target_root.join(profile).join(binary_file_name(bin_name))
}

pub fn binary_file_name(bin: &str) -> String {
    if cfg!(windows) {
        format!("{bin}.exe")
    } else {
        bin.to_string()
    }
}

/// The outcome of one one-shot RPC spawn.
#[derive(Debug)]
pub struct SpawnedResponse {
    /// The parsed first JSON line of stdout, when the protocol held.
    pub response: Option<Value>,
    /// Why no response exists (spawn failure, timeout, garbage stdout).
    pub transport_error: Option<String>,
}

/// Spawns the addon binary exactly the way the host does: one JSON request
/// line on stdin, one JSON response line on stdout, then the process exits.
/// Enforces a wall-clock timeout with kill — a hung addon cannot hang the
/// probe run.
pub fn run_addon_rpc(
    exe: &Path,
    fixture: &DadTestFixture,
    method: &str,
    auth: Option<&str>,
    timeout: Duration,
) -> SpawnedResponse {
    let mut params = json!({
        "tmdb_id": fixture.tmdb_id,
        "media_type": fixture.media_type.to_string(),
    });
    if let Some(season) = fixture.season {
        params["s"] = json!(season);
    }
    if let Some(episode) = fixture.episode {
        params["e"] = json!(episode);
    }
    if let Some(key) = auth {
        params["auth"] = json!(key);
    }
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "protocol_version": "2.0",
        "method": method,
        "params": params,
    });

    let mut child = match Command::new(exe)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            return SpawnedResponse {
                response: None,
                transport_error: Some(format!("failed to spawn {}: {e}", exe.display())),
            };
        }
    };

    {
        let mut stdin = child.stdin.take().expect("stdin was piped");
        let mut line = serde_json::to_string(&request).expect("request is serializable");
        line.push('\n');
        let _ = stdin.write_all(line.as_bytes());
        let _ = stdin.flush();
        // Dropping closes the pipe: the one-shot contract.
    }

    let mut stdout_pipe = child.stdout.take().expect("stdout was piped");
    let mut stderr_pipe = child.stderr.take().expect("stderr was piped");
    let stdout_reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = std::io::Read::read_to_string(&mut stdout_pipe, &mut text);
        text
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = std::io::Read::read_to_string(&mut stderr_pipe, &mut text);
        text
    });

    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    // Join readers so the pipes close cleanly before returning.
                    let _ = stdout_reader.join();
                    let _ = stderr_reader.join();
                    return SpawnedResponse {
                        response: None,
                        transport_error: Some(format!(
                            "addon did not exit within {} ms - killed",
                            timeout.as_millis()
                        )),
                    };
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => {
                return SpawnedResponse {
                    response: None,
                    transport_error: Some(format!("failed to wait on the addon: {e}")),
                };
            }
        }
    };

    let stdout = stdout_reader.join().unwrap_or_default();
    let _stderr = stderr_reader.join().unwrap_or_default();

    let first_line = stdout.lines().next();
    match first_line.and_then(|line| serde_json::from_str::<Value>(line).ok()) {
        Some(response) => SpawnedResponse { response: Some(response), transport_error: None },
        None => SpawnedResponse {
            response: None,
            transport_error: Some(format!(
                "no valid JSON response line on stdout (exit status {status:?})"
            )),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_path_honors_profile_and_exe_suffix() {
        let dir = Path::new(".");
        let path = artifact_path(dir, "release", "my-addon");
        let text = path.to_string_lossy().replace('\\', "/");
        assert!(text.ends_with("release/my-addon.exe") || text.ends_with("release/my-addon"));
        assert!(text.contains("release/"));
    }

    #[test]
    fn clippy_gate_flags_promote_both_deny_sets() {
        let args = clippy_args();
        assert_eq!(args[0], "clippy", "must invoke the clippy subcommand");
        assert!(
            args.contains(&"-D") && args.contains(&"warnings"),
            "every default clippy finding must be fatal: {args:?}"
        );
        assert!(
            args.contains(&"clippy::disallowed_methods"),
            "the clippy.toml denylist must be promoted to an error - that lint is \
             allow-by-default without it: {args:?}"
        );
    }
}