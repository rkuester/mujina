//! Daemon lifecycle management for mujina-miner.
//!
//! This module handles the core daemon functionality including initialization,
//! task management, signal handling, and graceful shutdown.

use tokio::signal::unix::{self, SignalKind};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

use crate::scheduler;
use crate::tracing::prelude::*;

/// The main daemon that coordinates all mining operations.
pub struct Daemon {
    shutdown: CancellationToken,
    tracker: TaskTracker,
}

impl Daemon {
    /// Create a new daemon instance.
    pub fn new() -> Self {
        Self {
            shutdown: CancellationToken::new(),
            tracker: TaskTracker::new(),
        }
    }

    /// Run the daemon until shutdown is requested.
    pub async fn run(self) -> anyhow::Result<()> {
        // Spawn the scheduler task
        self.tracker.spawn(scheduler::task(self.shutdown.clone()));
        self.tracker.close();

        info!("Started.");
        info!(
            "For hardware debugging, set RUST_LOG=mujina_miner=trace to see \
             all serial communication"
        );

        // Install signal handlers
        let mut sigint = unix::signal(SignalKind::interrupt())?;
        let mut sigterm = unix::signal(SignalKind::terminate())?;

        // Wait for shutdown signal
        tokio::select! {
            _ = sigint.recv() => {
                info!("Received SIGINT");
            },
            _ = sigterm.recv() => {
                info!("Received SIGTERM");
            },
        }

        // Initiate shutdown
        trace!("Shutting down.");
        self.shutdown.cancel();

        // Wait for all tasks to complete
        self.tracker.wait().await;
        info!("Exiting.");

        Ok(())
    }
}

impl Default for Daemon {
    fn default() -> Self {
        Self::new()
    }
}