//! Mining job source implementations.
//!
//! This module provides various sources for mining jobs, unifying different
//! methods of obtaining work for the mining hardware. Job sources can include:
//!
//! - **Pool Clients**: Connect to mining pools via protocols like Stratum v1/v2
//! - **Solo Mining**: Direct connection to Bitcoin nodes for solo mining
//! - **Dummy Work**: Generate synthetic jobs for power/thermal management
//!
//! # Architecture
//!
//! All job sources implement the [`JobSource`] trait, providing a consistent
//! interface for the scheduler to obtain and submit work regardless of the
//! underlying source.
//!
//! ## Key Components
//!
//! - [`JobSource`]: Core trait that all job sources must implement
//! - [`stratum_v1`]: Stratum v1 protocol client for pool mining
//! - [`stratum_v2`]: Stratum v2 protocol client (next-generation pool protocol)
//! - [`solo`]: Bitcoin node client for solo mining
//! - [`dummy`]: Synthetic job generator for power/thermal load management
//!
//! # Usage
//!
//! ```ignore
//! use mujina_miner::job_source::{JobSource, stratum_v1::StratumV1Client};
//!
//! // Create a job source
//! let mut source = StratumV1Client::new(config).await?;
//!
//! // Connect and authorize
//! source.connect().await?;
//!
//! // Get work
//! let job = source.get_job().await?;
//!
//! // Submit a solution
//! source.submit_share(share).await?;
//! ```

use std::fmt;

use crate::types::Extranonce2Error;
use async_trait::async_trait;

// Submodules
mod iterator;
mod job;

#[cfg(test)]
mod test_blocks;

// Re-export types from submodules
pub use iterator::{JobWork, JobWorkIterator};
pub use job::{MiningJob, Share};

// Re-export submodules once they're implemented
// pub mod stratum_v1;
// pub mod stratum_v2;
// pub mod solo;
// pub mod dummy;

/// Result type for job source operations.
pub type Result<T> = std::result::Result<T, JobSourceError>;

/// Errors that can occur in job source operations.
#[derive(Debug, thiserror::Error)]
pub enum JobSourceError {
    /// Network or connection error
    #[error("Connection error: {0}")]
    Connection(String),

    /// Protocol error (e.g., invalid message format)
    #[error("Protocol error: {0}")]
    Protocol(String),

    /// Authentication or authorization failure
    #[error("Authorization failed: {0}")]
    Authorization(String),

    /// No work available from source
    #[error("No work available")]
    NoWork,

    /// Submission rejected by source
    #[error("Share rejected: {0}")]
    ShareRejected(String),

    /// Configuration error
    #[error("Configuration error: {0}")]
    Configuration(String),

    /// I/O error
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Extranonce2 error
    #[error("Extranonce2 error: {0}")]
    Extranonce2(#[from] Extranonce2Error),

    /// Other errors
    #[error("{0}")]
    Other(String),
}

/// Core trait for all job sources.
///
/// This trait defines the interface that all job sources must implement,
/// allowing the scheduler to work with different sources uniformly.
#[async_trait]
pub trait JobSource: Send + Sync {
    /// Connect to the job source.
    ///
    /// This might mean connecting to a pool, a Bitcoin node, or
    /// initializing a dummy work generator.
    async fn connect(&mut self) -> Result<()>;

    /// Disconnect from the job source.
    async fn disconnect(&mut self) -> Result<()>;

    /// Check if connected to the source.
    fn is_connected(&self) -> bool;

    /// Get the next mining job.
    ///
    /// This may block until work is available. Returns `None` if
    /// the source is shutting down.
    async fn get_job(&mut self) -> Result<Option<MiningJob>>;

    /// Submit a share (solved work) to the source.
    ///
    /// Returns `true` if the share was accepted, `false` if rejected.
    async fn submit_share(&mut self, share: Share) -> Result<bool>;

    /// Get current difficulty target from the source.
    ///
    /// This is the minimum difficulty for shares to be accepted.
    async fn get_difficulty(&self) -> Result<f64>;

    /// Get source identifier for logging/monitoring.
    fn source_name(&self) -> &str;

    /// Get statistics from this job source.
    fn get_stats(&self) -> JobSourceStats;
}

/// Statistics for a job source.
#[derive(Debug, Clone, Default)]
pub struct JobSourceStats {
    /// Number of jobs received
    pub jobs_received: u64,

    /// Number of shares submitted
    pub shares_submitted: u64,

    /// Number of shares accepted
    pub shares_accepted: u64,

    /// Number of shares rejected
    pub shares_rejected: u64,

    /// Current difficulty
    pub current_difficulty: f64,

    /// Connection uptime in seconds
    pub uptime_seconds: u64,
}

impl fmt::Display for JobSourceStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Jobs: {}, Shares: {}/{} accepted, Difficulty: {:.2}, Uptime: {}s",
            self.jobs_received,
            self.shares_accepted,
            self.shares_submitted,
            self.current_difficulty,
            self.uptime_seconds
        )
    }
}
