//! `dad-local build` — the enforced release build.
//!
//! Pipeline: mandated-profile verification → clippy gate → cross-target
//! portability check (every platform the manifest declares) → `cargo build
//! --release` → artifact path. The SDK never hashes or signs: pinning the
//! artifact hash into the signed manifest is the team's internal publisher
//! tool's job.

use super::shared::{addon_dir, enforced_release_build, load_manifest, CmdResult};
use std::path::Path;

pub fn run(
    dir: Option<&Path>,
    target: Option<&str>,
    skip_portability: bool,
    skip_clippy: bool,
) -> CmdResult<()> {
    let dir = addon_dir(dir);
    let manifest = load_manifest(&dir)?;

    let artifact = enforced_release_build(&dir, &manifest, target, skip_portability, skip_clippy)?;

    println!(
        "\n──────────────────────────────────────────────────────────────\n \
         Build complete: {}\n──────────────────────────────────────────────────────────────",
        artifact.display()
    );
    println!(
        " The publisher tool pins the artifact hash into the signed manifest — the SDK does \
         not hash or sign."
    );
    Ok(())
}
