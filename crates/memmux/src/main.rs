//! `memmux` — the terminal UI client for the MemMux daemon.
//!
//! Phase 0 is a placeholder that reports the protocol version and points users at the daemon.
//! The dense Ratatui/Crossterm dashboard (Appendix A) is built in Phase 1 (SUM-18 epic).

use clap::Parser;

/// MemMux TUI client.
#[derive(Parser, Debug)]
#[command(name = "memmux", version, about = "MemMux terminal UI (Phase 1)")]
struct Cli {
    /// Print version information and exit.
    #[arg(long)]
    version_info: bool,
}

fn main() -> anyhow::Result<()> {
    let _cli = Cli::parse();
    println!(
        "memmux TUI {} — the dashboard arrives in Phase 1.",
        env!("CARGO_PKG_VERSION")
    );
    println!(
        "Speaks MemMux protocol {}. Start the daemon with `memmuxd`.",
        memmux_proto::PROTOCOL_VERSION
    );
    Ok(())
}
