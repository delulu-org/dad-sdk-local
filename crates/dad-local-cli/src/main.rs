//! # dad-local — the DAD local addon building kit
//!
//! Author-side tooling, and NOTHING more: scaffold (`init`), build with the
//! mandated release profile (`build`), iterate against fixture probes
//! (`dev`), the release conformance gate (`test`), and manifest schema
//! checking (`validate`).
//!
//! There is deliberately no `publish` command: signing and publishing belong
//! to the team's separate internal publisher tool, the same separation
//! dad-sdk 3.0.2 draws (its CLI is init/dev/test/validate only, and the
//! catalog module no longer lives in the SDK at all).

#![forbid(unsafe_code)]

mod commands;
mod profile;
mod spawn;
mod verdict;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Scaffold, build, and probe DAD local addons.
#[derive(Parser, Debug)]
#[command(name = "dad-local", version, about, disable_help_subcommand = true)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Scaffold a new local addon crate
    Init {
        /// Target directory for the new addon
        dir: PathBuf,
        /// Reverse-DNS addon id (default: org.example.{dir})
        #[arg(long)]
        id: Option<String>,
        /// Display name (default: directory name)
        #[arg(long)]
        name: Option<String>,
        /// Comma-separated capabilities: direct_stream,torrent,meta,subtitle
        #[arg(long, default_value = "direct_stream")]
        caps: String,
        /// Path to the dad_sdk_local workspace (default: resolved from this binary's build location)
        #[arg(long)]
        sdk_path: Option<PathBuf>,
    },
    /// Validate manifest.json against the DAD local contract
    Validate {
        /// Addon directory (default: current directory)
        dir: Option<PathBuf>,
    },
    /// Build the addon with the mandated release profile + portability check
    Build {
        /// Addon directory (default: current directory)
        dir: Option<PathBuf>,
        /// Build for a specific target triple instead of the host
        #[arg(long)]
        target: Option<String>,
        /// Skip the cross-target portability check (loudly warned)
        #[arg(long, default_value_t = false)]
        skip_portability: bool,
        /// Skip the clippy gate (loudly warned)
        #[arg(long, default_value_t = false)]
        skip_clippy: bool,
    },
    /// Run fixture probes against a debug build for quick iteration
    Dev {
        /// Addon directory (default: current directory)
        dir: Option<PathBuf>,
        /// How many fixtures to probe (default: 2)
        #[arg(long, default_value_t = 2)]
        fixtures: usize,
    },
    /// The release gate: build release + probe every fixture on every
    /// declared capability with pass/fail verdicts
    Test {
        /// Addon directory (default: current directory)
        dir: Option<PathBuf>,
        /// API key sent as `auth` on every probe (tests the authenticated path)
        #[arg(long)]
        key: Option<String>,
        /// Skip the clippy gate (loudly warned)
        #[arg(long, default_value_t = false)]
        skip_clippy: bool,
    },
}

fn main() {
    let cli = Cli::parse();
    let outcome = match cli.command {
        Commands::Init { dir, id, name, caps, sdk_path } => {
            commands::init::run(&dir, id.as_deref(), name.as_deref(), &caps, sdk_path.as_deref())
        }
        Commands::Validate { dir } => commands::validate::run(dir.as_deref()),
        Commands::Build { dir, target, skip_portability, skip_clippy } => commands::build::run(
            dir.as_deref(),
            target.as_deref(),
            skip_portability,
            skip_clippy,
        ),
        Commands::Dev { dir, fixtures } => commands::dev::run(dir.as_deref(), fixtures),
        Commands::Test { dir, key, skip_clippy } => {
            commands::test::run(dir.as_deref(), key.as_deref(), skip_clippy)
        }
    };
    if let Err(error) = outcome {
        eprintln!(" Error: {error}");
        std::process::exit(1);
    }
}
