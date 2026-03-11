//! Main entry point for the mujina-miner daemon.

use mujina_miner::{daemon::Daemon, tracing};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing::init();

    let daemon = Daemon::new();
    daemon.run().await
}
