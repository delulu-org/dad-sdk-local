//! Compile-level contract tests for `define_local_addon!`.
//!
//! The macro's entire value is compile-time: an invalid manifest or a
//! capability/handler mismatch must fail `cargo build`, never surface as a
//! runtime surprise. The macro reads `manifest.json` relative to
//! `CARGO_MANIFEST_DIR`, so in-memory fixtures (trybuild) cannot drive it;
//! instead we generate tiny throwaway addon crates and build them with the
//! real toolchain.
//!
//! All fixtures share one target directory so the dependency graph is
//! compiled once and reused across cases (and across reruns); each gets a
//! distinct package name so parallel builds cannot collide on one artifact.
//!
//! The build goes through `dad-local-toolchain`, the same cargo invocation the
//! CLI uses — so the fixtures see the machine's real build environment (the
//! `vcvarsall` one on Windows) no matter which shell launched `cargo test`.

use dad_local_toolchain::cargo_command;
use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    // .../dad_sdk_local/crates/dad-local-macros -> .../dad_sdk_local
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

fn valid_manifest(capabilities: &str) -> String {
    format!(
        r#"{{
  "id": "org.example.ui-fixture",
  "name": "UI Fixture",
  "version": "0.1.0",
  "type": "local",
  "protocol_version": "2.0",
  "capabilities": [{capabilities}],
  "platform_assets": {{
    "windows-x64": {{
      "download_url": "https://example.com/ui-fixture.exe",
      "binary_name": "ui-fixture.exe",
      "sha256": "",
      "entry_command": "rpc"
    }}
  }},
  "signature": ""
}}"#
    )
}

/// Materializes a throwaway addon crate: Cargo.toml (path deps on the real
/// SDK crates), manifest.json, and src/main.rs.
fn write_fixture(tag: &str, manifest: &str, main_rs: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dad-local-ui-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();

    let root = workspace_root();
    let core = root.join("crates").join("dad-local-core").display().to_string().replace('\\', "/");
    let runtime =
        root.join("crates").join("dad-local-runtime").display().to_string().replace('\\', "/");
    let name = format!("ui-fixture-{tag}");
    let cargo_toml = format!(
        r#"[package]
name = "{name}"
version = "0.0.0"
edition = "2021"

[workspace]

[dependencies]
dad-local-core = {{ path = "{core}" }}
dad-local-runtime = {{ path = "{runtime}" }}
serde = "1"
serde_json = "1"
tokio = {{ version = "1", default-features = false, features = ["rt", "net", "time", "io-util"] }}
"#,
    );
    std::fs::write(dir.join("Cargo.toml"), cargo_toml).unwrap();
    std::fs::write(dir.join("manifest.json"), manifest).unwrap();
    std::fs::write(dir.join("src/main.rs"), main_rs).unwrap();
    dir
}

/// Builds a fixture and returns `(succeeded, combined stderr + stdout)`.
fn build_fixture(dir: &Path) -> (bool, String) {
    let target_dir = workspace_root().join("target").join("ui-fixtures");
    let output = cargo_command(&["build", "--quiet"])
        .current_dir(dir)
        .env("CARGO_TARGET_DIR", &target_dir)
        .output()
        .expect("failed to spawn cargo for the UI fixture");
    let mut log = String::from_utf8_lossy(&output.stderr).into_owned();
    log.push_str(&String::from_utf8_lossy(&output.stdout));
    (output.status.success(), log)
}

const STREAMS_IMPL: &str = r#"
impl GetStreamsHandler for Addon {
    async fn get_streams(&self, _request: DadRequest) -> Result<Vec<StreamItem>, DadError> {
        Ok(vec![StreamItem::direct("Example", "https://cdn.example.com/s.m3u8")])
    }
}
"#;

const SUBTITLES_IMPL: &str = r#"
impl GetSubtitlesHandler for Addon {
    async fn get_subtitles(&self, _request: DadRequest) -> Result<Vec<SubtitleItem>, DadError> {
        Ok(Vec::new())
    }
}
"#;

fn main_rs(handlers: &str) -> String {
    format!(
        r#"use dad_local_runtime::prelude::*;

#[derive(Default)]
struct Addon;
{handlers}
dad_local_runtime::define_local_addon! {{
    manifest = "manifest.json";
    addon = Addon;
}}
"#
    )
}

#[test]
fn declared_capability_matching_its_handler_compiles() {
    let dir = write_fixture("pass", &valid_manifest("\"direct_stream\""), &main_rs(STREAMS_IMPL));
    let (ok, log) = build_fixture(&dir);
    assert!(ok, "a matching capability/handler pair must compile:\n{log}");
}

#[test]
fn declared_capability_without_its_handler_fails_to_compile() {
    // Manifest declares subtitle; the addon never implements GetSubtitlesHandler.
    let dir = write_fixture(
        "missing-handler",
        &valid_manifest("\"direct_stream\", \"subtitle\""),
        &main_rs(STREAMS_IMPL),
    );
    let (ok, log) = build_fixture(&dir);
    assert!(!ok, "a declared-but-unimplemented capability must fail the build");
    assert!(
        log.contains("GetSubtitlesHandler"),
        "the error must name the missing handler:\n{log}"
    );
}

#[test]
fn handler_without_its_declared_capability_fails_to_compile() {
    // The addon implements GetSubtitlesHandler but the manifest does not
    // declare `subtitle`, so the macro never emits HasCapability<Subtitles>.
    let dir = write_fixture(
        "undeclared-handler",
        &valid_manifest("\"direct_stream\""),
        &main_rs(&format!("{STREAMS_IMPL}{SUBTITLES_IMPL}")),
    );
    let (ok, log) = build_fixture(&dir);
    assert!(!ok, "an undeclared handler impl must fail the build");
    assert!(
        log.contains("HasCapability") || log.contains("Subtitles"),
        "the error must point at the missing capability marker:\n{log}"
    );
}

#[test]
fn invalid_manifest_is_a_compile_error() {
    let manifest = valid_manifest("\"direct_stream\"").replace("org.example.ui-fixture", "not-reverse-dns");
    let dir = write_fixture("bad-manifest", &manifest, &main_rs(STREAMS_IMPL));
    let (ok, log) = build_fixture(&dir);
    assert!(!ok, "an invalid manifest must fail the build");
    assert!(
        log.contains("violates the DAD local contract") && log.contains("reverse-DNS"),
        "the error must explain the contract violation:\n{log}"
    );
}