//! `dad-local init` — scaffold a new local addon crate.

use super::shared::{host_platform_key, CmdResult};
use dad_local_core::DadCapability;
use std::path::{Path, PathBuf};

struct Scaffold {
    dir_name: String,
    id: String,
    name: String,
    capabilities: Vec<DadCapability>,
    sdk_runtime_path: String,
    sdk_core_path: String,
}

/// Generates `src/main.rs` with handler impls for EXACTLY the declared
/// capabilities — the template must compile on first build, and the
/// capability↔handler compile-time check means a mismatched template would
/// not.
fn generate_main_rs(scaffold: &Scaffold) -> String {
    let has_streams = scaffold.capabilities.iter().any(|c| {
        matches!(c, DadCapability::DirectStream | DadCapability::Torrent)
    });
    let has_meta = scaffold.capabilities.iter().any(|c| matches!(c, DadCapability::Meta));
    let has_subtitles =
        scaffold.capabilities.iter().any(|c| matches!(c, DadCapability::Subtitle));

    let mut body = String::new();

    if has_streams {
        body.push_str(
            r#"impl GetStreamsHandler for Addon {
    async fn get_streams(&self, request: DadRequest) -> Result<Vec<StreamItem>, DadError> {
        // request.auth carries the user's API key (when the host's vault holds
        // one for this addon). Decide what it unlocks; raise DadError('unauthorized')
        // when the key is invalid, DadError('content_unavailable') when you have
        // nothing, DadError('upstream_unreachable') when your source is down.

        // TODO: replace the demo response with your resolver.
        if request.tmdb_id == 10378 {
            log!("Big Buck Bunny requested - fixtures are public-domain titles");
        }

        let mut headers = std::collections::BTreeMap::new();
        headers.insert("Referer".to_string(), "https://provider.example.com/".to_string());

        Ok(vec![
            StreamItem::direct("Example 1080p", "https://cdn.example.com/stream.m3u8"),
            StreamItem::proxied("Proxied example", "https://provider.example.com/stream.m3u8", headers),
        ])
    }
}

"#,
        );
    }

    if has_meta {
        body.push_str(
            r#"impl GetMetaHandler for Addon {
    async fn get_meta(&self, _request: DadRequest) -> Result<Option<MetaResponse>, DadError> {
        // Fill in ONLY what TMDB does not already carry: IMDb id/rating, trailers.
        // The first trailer URL is the default the client plays.
        Ok(Some(MetaResponse {
            imdb_id: Some("tt0111113".to_string()),
            imdb_rating: Some(8.8),
            trailers: Some(vec!["https://cdn.example.com/trailer.mp4".to_string()]),
        }))
    }
}

"#,
        );
    }

    if has_subtitles {
        body.push_str(
            r#"impl GetSubtitlesHandler for Addon {
    async fn get_subtitles(&self, request: DadRequest) -> Result<Vec<SubtitleItem>, DadError> {
        // request.auth is your upstream's key when the user provided one.
        Ok(vec![SubtitleItem {
            id: "en-sdh".to_string(),
            url: "https://cdn.example.com/en-sdh.vtt".to_string(),
            lang_code: "en".to_string(),
            language: "English".to_string(),
            title: "English [SDH]".to_string(),
            format: SubtitleFormat::Vtt,
            provider: None,
        }])
    }
}

"#,
        );
    }

    let caps_comment = scaffold
        .capabilities
        .iter()
        .map(|c| c.as_str())
        .collect::<Vec<_>>()
        .join(", ");

    format!(
        r#"//! {name} - a DAD local addon built with dad-local-runtime.
//!
//! One-shot execution model: the host spawns this binary, sends exactly one
//! JSON request line on stdin, reads one validated response line from stdout,
//! and the process exits. The SDK owns stdin/stdout; use `log!` for logging
//! (stderr) - never `println!`.
//!
//! Declared capabilities: {caps_comment}

#![forbid(unsafe_code)]

use dad_local_runtime::prelude::*;

#[derive(Default)]
struct Addon;

{body}
// The contract is enforced at COMPILE TIME:
// - every capability in manifest.json must have its handler impl above, and
// - every handler impl must have its capability declared in manifest.json.
// Remove one side and the build fails.
dad_local_runtime::define_local_addon! {{
    manifest = "manifest.json";
    addon = Addon;
}}
"#,
        name = scaffold.name,
        caps_comment = caps_comment,
        body = body,
    )
}

fn generate_manifest_json(scaffold: &Scaffold) -> String {
    let platform = host_platform_key();
    let binary = if cfg!(windows) {
        format!("{}.exe", scaffold.dir_name)
    } else {
        scaffold.dir_name.clone()
    };
    let caps = scaffold
        .capabilities
        .iter()
        .map(|c| format!("\"{}\"", c.as_str()))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        r#"{{
  "id": "{id}",
  "name": "{name}",
  "version": "0.1.0",
  "type": "local",
  "protocol_version": "2.0",

  "description": "TODO: one-line description of what this addon provides",
  "publisher": "TODO: your name or org",

  "capabilities": [{caps}],

  "platform_assets": {{
    "{platform}": {{
      "download_url": "https://your-release-host.example.com/{binary}",
      "binary_name": "{binary}",
      "sha256": "",
      "entry_command": "rpc"
    }}
  }},

  "signature": ""
}}
"#,
        id = scaffold.id,
        name = scaffold.name,
        caps = caps,
        platform = platform,
        binary = binary,
    )
}

/// The scaffolded lint denylist, enforced by the clippy gate in
/// `dad-local build` and `dad-local test`.
///
/// A local addon is a stateless resolver: one request in, one response out.
/// Files, processes, environment variables, and raw sockets are host
/// concerns - there is no legitimate reason for addon code to touch them,
/// so clippy turns each use into a build failure.
const CLIPPY_TOML: &str = r#"# Enforced by `dad-local build` / `dad-local test` (clippy gate).
# A local addon is a stateless resolver - one request in, one response out.
# Files, processes, environment, and raw sockets are host concerns.
disallowed-methods = [
    "std::fs::read",
    "std::fs::read_to_string",
    "std::fs::write",
    "std::fs::File::create",
    "std::process::Command::new",
    "std::env::var",
    "std::env::var_os",
    "std::net::TcpStream::connect",
]
"#;

fn generate_cargo_toml(scaffold: &Scaffold) -> String {
    format!(
        r#"[package]
name = "{crate_name}"
version = "0.1.0"
edition = "2021"
license = "MIT"

# Standalone crate: the empty [workspace] table detaches this crate from any
# parent workspace, guaranteeing the [profile.release] below governs the build.
[workspace]

[dependencies]
# TEMPORARY: local path dependencies until the SDK is published (crates.io or
# git). Once published, replace both lines with version dependencies.
dad-local-core = {{ path = "{core_path}" }}
dad-local-runtime = {{ path = "{runtime_path}" }}
serde = "1"
serde_json = "1"
tokio = {{ version = "1", default-features = false, features = ["rt", "net", "time", "io-util"] }}

# MANDATED release profile - `dad-local build` and `dad-local test` REFUSE to
# run without exactly this block, and `panic` must stay unwind (the runtime
# catches handler panics; abort would kill the protocol).
[profile.release]
opt-level = 3
lto = true
codegen-units = 1
strip = true
"#,
        crate_name = scaffold.dir_name,
        core_path = scaffold.sdk_core_path,
        runtime_path = scaffold.sdk_runtime_path,
    )
}

fn generate_readme(scaffold: &Scaffold) -> String {
    format!(
        r#"# {name}

A DAD **local** addon - a native binary the Delulu host spawns as a child
process and speaks one-shot JSON-RPC over stdio with.

## Develop

```bash
dad-local dev       # debug build + fixture probes, for iteration
dad-local test      # release build + the full pre-ship conformance gate
dad-local validate  # manifest.json schema check
dad-local build     # enforced release build (profile + portability)
```

## Declared capabilities

{caps}

Edit `manifest.json` and `src/main.rs` together - the compile-time
capability check fails the build when they disagree.

## Shipping

`dad-local build` produces the release artifact. Hashing and signing happen
in the team's separate publisher pipeline - the SDK is a building kit,
nothing more.
"#,
        name = scaffold.name,
        caps = scaffold
            .capabilities
            .iter()
            .map(|c| c.as_str())
            .collect::<Vec<_>>()
            .join(", "),
    )
}


fn write_file(path: &Path, content: &str) -> CmdResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("failed to create {}: {e}", parent.display()))?;
    }
    std::fs::write(path, content).map_err(|e| format!("failed to write {}: {e}", path.display()))
}

pub fn run(
    dir: &Path,
    id: Option<&str>,
    name: Option<&str>,
    caps: &str,
    sdk_path: Option<&Path>,
) -> CmdResult<()> {
    let dir_name = dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "my-addon".to_string());

    if dir.exists() && std::fs::read_dir(dir).map(|mut d| d.next().is_some()).unwrap_or(false) {
        return Err(format!("{} already exists and is not empty", dir.display()));
    }

    let capabilities = caps
        .split(',')
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
        .map(|c| serde_json::from_value::<DadCapability>(serde_json::Value::String(c)))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| {
            "'--caps' accepts only: direct_stream, torrent, meta, subtitle (comma-separated)"
                .to_string()
        })?;
    if capabilities.is_empty() {
        return Err("'--caps' must declare at least one capability".to_string());
    }

    let id = match id {
        Some(id) => id.to_string(),
        None => format!(
            "org.example.{}",
            dir_name.to_lowercase().replace(|c: char| !c.is_ascii_alphanumeric(), "-")
        ),
    };

    // SDK paths for the path-dependencies: --sdk-path wins, else resolve from
    // this binary's build location (works for local development builds).
    let sdk_root: PathBuf = match sdk_path {
        Some(p) => p.to_path_buf(),
        None => Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .ok_or_else(|| "could not resolve the SDK root - pass --sdk-path".to_string())?,
    };
    let sdk_runtime_path = sdk_root.join("crates").join("dad-local-runtime");
    let sdk_core_path = sdk_root.join("crates").join("dad-local-core");
    for path in [&sdk_runtime_path, &sdk_core_path] {
        if !path.join("Cargo.toml").exists() {
            return Err(format!(
                "SDK crate not found at {} - pass --sdk-path pointing at the dad_sdk_local workspace",
                path.display()
            ));
        }
    }

    let scaffold = Scaffold {
        dir_name: dir_name.clone(),
        id,
        name: name.unwrap_or(&dir_name).to_string(),
        capabilities,
        sdk_runtime_path: sdk_runtime_path.to_string_lossy().replace('\\', "/"),
        sdk_core_path: sdk_core_path.to_string_lossy().replace('\\', "/"),
    };

    std::fs::create_dir_all(dir).map_err(|e| format!("failed to create {}: {e}", dir.display()))?;
    write_file(&dir.join("Cargo.toml"), &generate_cargo_toml(&scaffold))?;
    write_file(&dir.join("manifest.json"), &generate_manifest_json(&scaffold))?;
    write_file(&dir.join("src").join("main.rs"), &generate_main_rs(&scaffold))?;
    write_file(&dir.join("README.md"), &generate_readme(&scaffold))?;
    write_file(&dir.join("clippy.toml"), CLIPPY_TOML)?;
    write_file(&dir.join(".gitignore"), "/target\n")?;
    println!("\n Created local addon '{}' in {}", scaffold.name, dir.display());
    println!(" Addon id: {}", scaffold.id);
    println!(
        " Capabilities: {}",
        scaffold.capabilities.iter().map(|c| c.as_str()).collect::<Vec<_>>().join(", ")
    );
    println!("\n manifest.json - every field you control:");
    println!("   id              reverse-DNS - do not rename after publishing");
    println!("   name            display name in the client");
    println!("   version         strict major.minor.patch - the client's source of truth");
    println!("   protocol_version  \"2.0\" - the wire protocol this binary speaks");
    println!("   capabilities    {caps_comment}", caps_comment = scaffold.capabilities.iter().map(|c| c.as_str()).collect::<Vec<_>>().join(", "));
    println!("   platform_assets per-{{os}}-{{arch}} binaries; sha256 stays \"\" until publish");
    println!("   api_key         optional {{ required, page_url }} gate - enforced before your handler runs");
    println!("\n Still TODO in your scaffold: description, publisher");
    println!("\n Next steps:");
    println!("   cd {}", dir.display());
    println!("   dad-local dev        # iterate: debug build + fixture probes");
    println!("   dad-local test       # pre-ship gate: release build + full probes");
    println!("   dad-local build      # enforced release build");
    println!();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dad-local-init-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn init_generates_a_valid_compiling_shaped_addon() {
        let dir = tmp_dir("valid");
        run(&dir, Some("org.example.my-addon"), Some("My Addon"), "direct_stream,meta,subtitle", None)
            .expect("init must succeed");

        // The generated manifest passes the FULL contract validator.
        let raw: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("manifest.json")).unwrap(),
        )
        .unwrap();
        let manifest = dad_local_core::LocalAddonManifest::parse(&raw)
            .expect("generated manifest must be contract-valid");
        assert_eq!(manifest.id, "org.example.my-addon");
        assert_eq!(manifest.capabilities.len(), 3);
        assert!(manifest.signature.is_empty(), "scaffold is authoring state");

        // Handlers generated for EXACTLY the declared capabilities.
        let main_rs = std::fs::read_to_string(dir.join("src").join("main.rs")).unwrap();
        for expected in ["GetStreamsHandler", "GetMetaHandler", "GetSubtitlesHandler"] {
            assert!(main_rs.contains(expected), "missing {expected} in template");
        }

        // The generated Cargo.toml satisfies the mandated-profile checker.
        let cargo_toml = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap();
        let errors = crate::profile::check_release_profile(&cargo_toml);
        assert!(errors.is_empty(), "scaffold profile must be compliant: {errors:?}");
        assert!(cargo_toml.contains("[workspace]"), "scaffold must be detached from parent workspaces");

        // The scaffold ships the clippy denylist the build gate enforces.
        let clippy_toml = std::fs::read_to_string(dir.join("clippy.toml")).unwrap();
        assert!(clippy_toml.contains("disallowed-methods"), "clippy.toml must declare a denylist");
        for banned in [
            "std::fs::read",
            "std::fs::write",
            "std::process::Command::new",
            "std::env::var",
            "std::net::TcpStream::connect",
        ] {
            assert!(clippy_toml.contains(banned), "clippy.toml must ban {banned}");
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn init_generates_only_declared_handlers() {
        let dir = tmp_dir("subs-only");
        run(&dir, None, None, "subtitle", None).expect("init must succeed");
        let main_rs = std::fs::read_to_string(dir.join("src").join("main.rs")).unwrap();
        assert!(main_rs.contains("GetSubtitlesHandler"));
        assert!(!main_rs.contains("GetStreamsHandler"), "undeclared handler must not be templated");
        assert!(!main_rs.contains("GetMetaHandler"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn init_refuses_bad_caps_and_nonempty_dirs() {
        let dir = tmp_dir("badcaps");
        let error = run(&dir, None, None, "direct_stream,warp_drive", None).unwrap_err();
        assert!(error.contains("--caps"), "{error}");

        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("something.txt"), "x").unwrap();
        let error = run(&dir, None, None, "subtitle", None).unwrap_err();
        assert!(error.contains("not empty"), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
