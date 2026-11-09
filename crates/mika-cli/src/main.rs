//! Mika CLI — P2P-only node mesh client.
//!
//! Nodes are pure libp2p peers (no HTTP API, no web UI). The CLI pushes
//! slivers to ONE entry peer over the store protocol
//! (`/mika/sliver-store/1.0.0`) and the mesh disseminates them gossipsub +
//! fetch. `--nodes` takes `host:port` (e.g. `127.0.0.1:34001`, resolved via
//! `./.mesh` or `<MIKA_HOME>/mesh`) or a full multiaddr.
//!
//! Layout: command tree in `commands/` (kanari-style groups), handlers in
//! `commands/{storage,network,committee}.rs`, this file only dispatches.

pub mod commands;

use anyhow::Result;
use clap::Parser;

use commands::{Commands, NetworkCmd};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = commands::Cli::parse();

    match cli.command {
        Commands::Committee { cmd } => commands::committee::run(cmd).await?,
        Commands::Storage { cmd } => commands::storage::run(cmd).await?,
        Commands::Network {
            cmd:
                NetworkCmd::Start {
                    node_id,
                    data_dir,
                    no_p2p,
                    p2p_listen,
                    bootstrap,
                    committee_signers,
                },
        } => {
            commands::network::start(
                node_id,
                data_dir,
                no_p2p,
                p2p_listen,
                bootstrap,
                committee_signers,
            )
            .await?
        }
    }
    Ok(())
}
