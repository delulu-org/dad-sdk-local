//! `dad-local test` — the pre-ship gate.
//!
//! Builds the addon through the enforced release pipeline, then probes every
//! declared capability against the public-domain fixtures, classifying each
//! response with the same asymmetric bias as `dad test` for HTTP addons:
//! valid payloads and graceful errors pass; server/contract errors fail;
//! a run where nothing ever returned data fails; a run answered entirely by
//! a declared api_key gate passes narrowly with a re-run hint.

use super::shared::{addon_dir, enforced_release_build, load_manifest, probe_methods_for, CmdResult};
use dad_local_core::DAD_TEST_FIXTURES;
use std::path::Path;
use std::time::Duration;

const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

struct ProbeOutcome {
    ok: bool,
    gate_only: bool,
    produced_data: bool,
    line: String,
    detail: Option<String>,
}

pub fn run(dir: Option<&Path>, key: Option<&str>, skip_clippy: bool) -> CmdResult<()> {
    let dir = addon_dir(dir);
    let manifest = load_manifest(&dir)?;
    let declares_gate = manifest.api_key.is_some();

    println!(" enforced release build (profile + clippy + portability) ...");
    let exe = enforced_release_build(&dir, &manifest, None, false, skip_clippy)?;

    let methods = probe_methods_for(&manifest);
    if methods.is_empty() {
        return Err("the manifest declares no probeable capability (direct_stream, torrent, meta, subtitle)".to_string());
    }

    println!(
        "\n{}\n DAD test - {} ({})\n{}\n",
        "-".repeat(64),
        manifest.name,
        manifest.id,
        "-".repeat(64)
    );
    println!(" Capabilities: {}", super::shared::caps_label(&manifest.capabilities));
    println!(
        " Auth: {}",
        match key {
            Some(_) => "sending Authorization-style `auth` on every probe (authenticated path)".to_string(),
            None => "none (graceful-rejection path - pass --key <api-key> to test as an authenticated caller)"
                .to_string(),
        }
    );

    let mut outcomes: Vec<ProbeOutcome> = Vec::new();
    let allowed_note = format!(
        "declared: {}",
        super::shared::caps_label(&manifest.capabilities)
    );

    for fixture in &DAD_TEST_FIXTURES {
        for method in &methods {
            let spawned =
                crate::spawn::run_addon_rpc(&exe, fixture, method, key, PROBE_TIMEOUT);
            let verdict = crate::verdict::classify(method, &manifest.capabilities, declares_gate, key.is_some(), &spawned);
            let route = format!("{method}/{} {}", fixture.media_type, fixture.tmdb_id);
            let line = format!(
                " [{}] {} - {route} ({})",
                if verdict.ok { "OK" } else { "FAIL" },
                fixture.title,
                allowed_note
            );
            outcomes.push(ProbeOutcome {
                ok: verdict.ok,
                gate_only: verdict.gate_only,
                produced_data: verdict.produced_data,
                line,
                detail: if verdict.detail.is_empty() { None } else { Some(verdict.detail) },
            });
        }
    }

    for outcome in &outcomes {
        println!("{}", outcome.line);
        if let Some(detail) = &outcome.detail {
            println!("       {detail}");
        }
    }

    let failed = outcomes.iter().filter(|o| !o.ok).count();
    let gate_only_count = outcomes.iter().filter(|o| o.gate_only).count();
    let total = outcomes.len();

    if failed > 0 {
        println!("\n FAIL - {failed}/{total} probes failed. Fix, rebuild, and re-run `dad-local test`.");
        std::process::exit(1);
    }

    let produced_any = outcomes.iter().any(|o| o.produced_data);
    if !produced_any {
        if gate_only_count == total && key.is_none() {
            println!(
                "\n PASS - {total} probes, all clean, but every one was a rejected anonymous request: \
                 the declared api_key gate works. Nothing else was exercised."
            );
            println!(" Re-run with --key <your-key> to test the addon's actual data.");
            return Ok(());
        }
        println!(
            "\n FAIL - {total} probes were all contract-valid, but NOT ONE returned any data \
             (no streams, no meta, no subtitles for any fixture)."
        );
        println!(" An addon that answers correctly with nothing is still broken.");
        std::process::exit(1);
    }

    println!("\n PASS - {total} probes, all clean.");
    Ok(())
}
