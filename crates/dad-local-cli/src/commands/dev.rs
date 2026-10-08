//! `dad-local dev` — quick iteration: debug build + a few fixture probes with
//! full request/response dumps, so the author sees the exact wire traffic the
//! host will produce.

use super::shared::{addon_dir, bin_name, load_manifest, probe_methods_for, CmdResult};
use dad_local_core::DAD_TEST_FIXTURES;
use std::path::Path;
use std::time::Duration;

const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

pub fn run(dir: Option<&Path>, fixture_count: usize) -> CmdResult<()> {
    let dir = addon_dir(dir);
    let manifest = load_manifest(&dir)?;

    println!(" cargo build (debug) ...");
    crate::spawn::cargo(&dir, &["build", "--quiet"])?;
    let name = bin_name(&dir)?;
    let exe = crate::spawn::artifact_path(&dir, "debug", &name);
    if !exe.exists() {
        return Err(format!("debug build finished but the binary is missing at {}", exe.display()));
    }

    let methods = probe_methods_for(&manifest);
    if methods.is_empty() {
        return Err("the manifest declares no probeable capability (direct_stream, torrent, meta, subtitle)".to_string());
    }

    println!(
        "\n{}\n DAD dev - {} ({})\n{}\n",
        "-".repeat(64),
        manifest.name,
        manifest.id,
        "-".repeat(64)
    );
    println!(" Capabilities: {}", super::shared::caps_label(&manifest.capabilities));
    println!(" Binary: {}", exe.display());

    let fixtures = DAD_TEST_FIXTURES.iter().take(fixture_count.max(1));
    for fixture in fixtures {
        for method in &methods {
            let spawned = crate::spawn::run_addon_rpc(&exe, fixture, method, None, PROBE_TIMEOUT);
            let params = if fixture.season.is_some() {
                format!(
                    "{} {} s{}e{}",
                    fixture.media_type,
                    fixture.tmdb_id,
                    fixture.season.map_or("?".to_string(), |s| s.to_string()),
                    fixture.episode.map_or("?".to_string(), |e| e.to_string())
                )
            } else {
                format!("{} {}", fixture.media_type, fixture.tmdb_id)
            };
            println!("\n -> {method}/{params}   ({})", fixture.title);
            match spawned.response {
                Some(response) => {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&response).unwrap_or_else(|_| response.to_string())
                    )
                }
                None => println!(
                    " TRANSPORT FAILURE: {}",
                    spawned.transport_error.as_deref().unwrap_or("unknown")
                ),
            }
        }
    }

    println!("\n Iterating? Edit src/main.rs and run `dad-local dev` again.");
    println!(" Ready to ship? `dad-local test` is the pre-ship gate.\n");
    Ok(())
}
