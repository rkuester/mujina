//! API data transfer objects.
//!
//! These types define the API contract shared between the server and
//! clients.

use serde::Serialize;

/// Full miner state snapshot.
#[derive(Clone, Debug, Default, Serialize)]
pub struct MinerState {
    pub uptime_secs: u64,
    /// Aggregate hashrate in hashes per second.
    pub hashrate: u64,
    pub shares_submitted: u64,
    pub boards: Vec<BoardState>,
    pub sources: Vec<SourceState>,
}

/// Board status.
#[derive(Clone, Debug, Serialize)]
pub struct BoardState {
    pub name: String,
    pub threads: Vec<ThreadState>,
}

/// Per-thread runtime status.
#[derive(Clone, Debug, Serialize)]
pub struct ThreadState {
    pub name: String,
    /// Hashrate in hashes per second.
    pub hashrate: u64,
    pub is_active: bool,
}

/// Job source status.
#[derive(Clone, Debug, Serialize)]
pub struct SourceState {
    pub name: String,
}
