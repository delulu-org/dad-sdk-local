//! Logic shared by several commands: addon-dir resolution, manifest loading,
//! platform→triple mapping, and the enforced release-build pipeline.

use dad_local_core::{DadCapability, LocalAddonManifest};
use std::path::{Path, PathBuf};

pub type CmdResult<T> = Result<T, String>;

/// Resolves the addon directory (`None` = current directory).
pub fn addon_dir(dir: Option<&Path>) -> PathBuf {
    dir.map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."))
}

/// Loads + fully validates the addon's manifest.json.
pub fn load_manifest(addon_dir: &Path) -> CmdResult<LocalAddonManifest> {
    let path = addon_dir.join("manifest.json");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("{} is not valid JSON: {e}", path.display()))?;
    LocalAddonManifest::parse(&value)
        .map_err(|errors| format!("manifest.json violates the DAD local contract:\n  - {}", errors.join("\n  - ")))
}

/// Reads the addon's package name from Cargo.toml — the built binary's name.
pub fn bin_name(addon_dir: &Path) -> CmdResult<String> {
    let text = std::fs::read_to_string(addon_dir.join("Cargo.toml"))
        .map_err(|e| format!("failed to read {}/Cargo.toml: {e}", addon_dir.display()))?;
    let doc: toml::Value =
        toml::from_str(&text).map_err(|e| format!("Cargo.toml is not valid TOML: {e}"))?;
    doc.get("package")
        .and_then(|p| p.get("name"))
        .and_then(toml::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "Cargo.toml is missing [package].name".to_string())
}

/// Platform key → Rust target triple. Unknown keys fail loudly instead of
/// being silently skipped.
pub fn platform_to_triple(platform: &str) -> Option<&'static str> {
    Some(match platform {
        "windows-x64" => "x86_64-pc-windows-msvc",
        "windows-x86" => "i686-pc-windows-msvc",
        "windows-arm64" => "aarch64-pc-windows-msvc",
        "linux-x64" => "x86_64-unknown-linux-gnu",
        "linux-arm64" => "aarch64-unknown-linux-gnu",
        "macos-x64" => "x86_64-apple-darwin",
        "macos-arm64" => "aarch64-apple-darwin",
        _ => return None,
    })
}

/// The host's platform key (what the HOST would look up in platform_assets).
pub fn host_platform_key() -> String {
    let arch = if cfg!(target_arch = "x86_64") {
        "x64"
    } else if cfg!(target_arch = "x86") {
        "x86"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "unknown"
    };
    format!("{}-{arch}", std::env::consts::OS)
}

/// The full enforced release pipeline shared by `build` and `test`:
/// mandated-profile check → clippy gate → portability check (every declared
/// target) → `cargo build --release`. Returns the artifact path.
pub fn enforced_release_build(
    addon_dir: &Path,
    manifest: &LocalAddonManifest,
    target: Option<&str>,
    skip_portability: bool,
    skip_clippy: bool,
) -> CmdResult<PathBuf> {
    let cargo_toml = std::fs::read_to_string(addon_dir.join("Cargo.toml"))
        .map_err(|e| format!("failed to read {}/Cargo.toml: {e}", addon_dir.display()))?;
    let profile_errors = crate::profile::check_release_profile(&cargo_toml);
    if !profile_errors.is_empty() {
        return Err(format!(
            "the mandated release profile is not satisfied:\n  - {}",
            profile_errors.join("\n  - ")
        ));
    }
    println!(" [OK] mandated release profile verified");

    if skip_clippy {
        println!(" [--] WARNING: clippy gate skipped - the lint denylist in clippy.toml will not be enforced");
    } else {
        println!(" [..] clippy gate");
        crate::spawn::clippy_gate(addon_dir)?;
    }

    if skip_portability {
        println!(" [--] WARNING: portability check skipped - platform-specific code will not be caught here");
    } else {
        let triples: Vec<(&str, &str)> = manifest
            .platform_assets
            .keys()
            .map(|platform| {
                let triple = platform_to_triple(platform).ok_or_else(|| {
                    format!("unknown platform key '{platform}' in platform_assets")
                })?;
                Ok((platform.as_str(), triple))
            })
            .collect::<Result<Vec<(&str, &str)>, String>>()?;
        for (platform, triple) in triples {
            println!(" [..] portability check: {platform} ({triple})");
            crate::spawn::check_target(addon_dir, triple).map_err(|e| {
                format!(
                    "portability check failed for {triple} - the addon must compile clean on \
                     EVERY platform it declares:\n{e}\n(hint: is the target installed? \
                     `rustup target add {triple}`; use --skip-portability to bypass knowingly)"
                )
            })?;
            println!(" [OK] {platform} compiles clean");
        }
    }

    let mut args = vec!["build", "--quiet", "--release"];
    if let Some(triple) = target {
        args.push("--target");
        args.push(triple);
    }
    crate::spawn::cargo(addon_dir, &args)?;

    let name = bin_name(addon_dir)?;
    let artifact = if let Some(triple) = target {
        let target_root = std::env::var("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| addon_dir.join("target"));
        target_root.join(triple).join("release").join(crate::spawn::binary_file_name(&name))
    } else {
        crate::spawn::artifact_path(addon_dir, "release", &name)
    };
    if !artifact.exists() {
        return Err(format!("build succeeded but the artifact is missing at {}", artifact.display()));
    }
    Ok(artifact)
}

/// Which declared capabilities map to which probe methods — used by `dev`
/// and `test` alike, derived once and shared so the two can never drift.
pub fn probe_methods_for(manifest: &LocalAddonManifest) -> Vec<&'static str> {
    crate::verdict::probe_methods(&manifest.capabilities)
}

/// Pretty capability list for banners.
pub fn caps_label(capabilities: &[DadCapability]) -> String {
    capabilities.iter().map(|c| c.as_str()).collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_mapping_covers_the_seven_known_keys() {
        assert_eq!(platform_to_triple("windows-x64"), Some("x86_64-pc-windows-msvc"));
        assert_eq!(platform_to_triple("macos-arm64"), Some("aarch64-apple-darwin"));
        assert_eq!(platform_to_triple("linux-x86"), None, "unknown combos must fail loudly");
    }
}
