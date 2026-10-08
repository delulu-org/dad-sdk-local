//! `dad-local validate` — manifest.json schema check.

use super::shared::{addon_dir, load_manifest, CmdResult};
use std::path::Path;

pub fn run(dir: Option<&Path>) -> CmdResult<()> {
    let dir = addon_dir(dir);
    let manifest = load_manifest(&dir)?;
    println!(" manifest.json is valid!");
    println!(
        "   id: {}  version: {}  protocol: {}",
        manifest.id, manifest.version, manifest.protocol_version
    );
    println!(
        "   capabilities: {}",
        super::shared::caps_label(&manifest.capabilities)
    );
    println!(
        "   platform assets: {}",
        manifest
            .platform_assets
            .keys()
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    );
    Ok(())
}
