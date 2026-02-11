//! API data transfer objects.
//!
//! These types define the API contract shared between the server and
//! clients.

use serde::{Deserialize, Serialize};

/// Full miner state snapshot.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct MinerState {
    pub uptime_secs: u64,
    /// Aggregate hashrate in hashes per second.
    pub hashrate: u64,
    pub shares_submitted: u64,
    pub boards: Vec<BoardState>,
    pub sources: Vec<SourceState>,
}

/// Board status.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct BoardState {
    /// URL-friendly identifier (e.g. "bitaxe-e2f56f9b").
    pub name: String,
    pub model: String,
    pub serial: Option<String>,
    pub fans: Vec<Fan>,
    pub temperatures: Vec<TemperatureSensor>,
    pub powers: Vec<PowerMeasurement>,
    pub threads: Vec<ThreadState>,
}

/// Fan status.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Fan {
    pub name: String,
    /// Measured RPM, or null if the tachometer read failed.
    pub rpm: Option<u32>,
    /// Measured duty cycle, or null if the read failed.
    pub percent: Option<u8>,
    /// Target duty cycle, or null if the fan is in automatic mode.
    pub target_percent: Option<u8>,
}

/// Temperature sensor reading.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TemperatureSensor {
    pub name: String,
    pub temperature_c: Option<f32>,
}

/// Voltage, current, and power from a single measurement point.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PowerMeasurement {
    pub name: String,
    pub voltage_v: Option<f32>,
    pub current_a: Option<f32>,
    pub power_w: Option<f32>,
}

/// Per-thread runtime status.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ThreadState {
    pub name: String,
    /// Hashrate in hashes per second.
    pub hashrate: u64,
    pub is_active: bool,
}

/// Job source status.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SourceState {
    pub name: String,
}
