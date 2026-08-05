//! `memmuxd` — the MemMux control-plane daemon.
//!
//! Phase 0 ships a minimal binary that exposes the process-accounting sampler on the command
//! line so the instrumentation can be exercised end to end. The authoritative daemon (gRPC
//! server, scheduler, lifecycle manager, SQLite store) is built out in Phase 1.

use clap::{Parser, Subcommand};

/// MemMux daemon CLI.
#[derive(Parser, Debug)]
#[command(name = "memmuxd", version, about = "MemMux control-plane daemon")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Print the daemon and protocol versions and exit.
    Version,
    /// Take a one-shot snapshot of the current process tree and print attribution.
    Snapshot {
        /// Root pid to attribute against (defaults to this process).
        #[arg(long)]
        root: Option<i32>,
    },
}

fn main() -> anyhow::Result<()> {
    // A simple level filter driven by RUST_LOG (INFO by default). Full `EnvFilter` directives
    // arrive in Phase 1 once the daemon's observability surface justifies the extra deps.
    let level = std::env::var("RUST_LOG")
        .ok()
        .and_then(|s| s.parse::<tracing::Level>().ok())
        .unwrap_or(tracing::Level::INFO);
    tracing_subscriber::fmt().with_max_level(level).init();

    let cli = Cli::parse();
    match cli.command {
        Command::Version => {
            println!(
                "memmuxd {} (protocol {})",
                env!("CARGO_PKG_VERSION"),
                memmux_proto::PROTOCOL_VERSION
            );
        }
        Command::Snapshot { root } => {
            let sampler = memmux_metrics::default_sampler();
            let snapshot = sampler.snapshot()?;
            let root = root.unwrap_or_else(|| std::process::id() as i32);
            let tree = memmux_metrics::ProcessTree::from_samples(snapshot.samples);
            let total = tree.subtree_rss_bytes(root);
            println!(
                "root pid {root}: {} descendants, subtree RSS {:.1} MiB",
                tree.descendants(root).len(),
                total as f64 / (1024.0 * 1024.0)
            );
        }
    }
    Ok(())
}
