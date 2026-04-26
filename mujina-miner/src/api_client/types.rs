//! API data transfer objects.
//!
//! These types define the API contract shared between the server and
//! clients (CLI, TUI). See `docs/api.md` (at the repository root)
//! for the full API contract documentation, including conventions
//! for null values and units.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::types::Temperature;

/// Full miner telemetry snapshot.
#[derive(Clone, Debug, Default, Deserialize, Serialize, ToSchema)]
pub struct MinerTelemetry {
    pub uptime_secs: u64,
    /// Aggregate hashrate in hashes per second.
    pub hashrate: u64,
    pub shares_submitted: u64,
    pub paused: bool,
    pub boards: Vec<BoardTelemetry>,
    pub sources: Vec<SourceTelemetry>,
}

/// Board telemetry snapshot.
#[derive(Clone, Debug, Default, Deserialize, Serialize, ToSchema)]
pub struct BoardTelemetry {
    /// URL-friendly identifier (e.g. "bitaxe-e2f56f9b").
    pub name: String,
    pub model: String,
    pub serial: Option<String>,
    pub fans: Vec<Fan>,
    pub temperatures: Vec<TemperatureSensor>,
    pub powers: Vec<PowerMeasurement>,
    pub threads: Vec<ThreadTelemetry>,
}

/// Fan status.
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
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
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
pub struct TemperatureSensor {
    pub name: String,
    #[serde(rename = "temperature_c")]
    #[schema(value_type = Option<f32>)]
    pub temperature: Option<Temperature>,
}

/// Voltage, current, and power from a single measurement point.
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
pub struct PowerMeasurement {
    pub name: String,
    pub voltage_v: Option<f32>,
    pub current_a: Option<f32>,
    pub power_w: Option<f32>,
}

/// Per-thread telemetry.
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
pub struct ThreadTelemetry {
    pub name: String,
    /// Hashrate in hashes per second.
    pub hashrate: u64,
    pub is_active: bool,
}

/// Writable fields for `PATCH /api/v0/miner`.
///
/// All fields are optional; only those present in the request body are
/// applied. Read-only fields like `uptime_secs` and `hashrate` are not
/// included and cannot be set.
#[derive(Clone, Debug, Default, Deserialize, Serialize, ToSchema)]
pub struct MinerPatchRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paused: Option<bool>,
}

/// Request body for setting a fan's target duty cycle.
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
pub struct SetFanTargetRequest {
    /// Target duty cycle percentage (0--100), or null for automatic control.
    pub target_percent: Option<u8>,
}

/// Job source telemetry.
#[derive(Clone, Debug, Default, Deserialize, Serialize, ToSchema)]
pub struct SourceTelemetry {
    pub name: String,
    /// Connection URL (e.g. "stratum+tcp://pool:3333"), if applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Current share difficulty set by the source.
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_opt_f64_as_integer_when_whole"
    )]
    pub difficulty: Option<f64>,
    /// Snapshot of the block currently being mined (only populated
    /// for sources that build their own coinbase, like the
    /// getblocktemplate path). `None` for Stratum and dummy sources.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block: Option<BlockInProgress>,
}

/// Snapshot of the block currently being mined by a getblocktemplate
/// source. The shape mirrors what the on-device display surfaces:
/// who's getting paid, how full the block is, what's the most
/// interesting tx in it, and where the block sits in monetary time.
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
pub struct BlockInProgress {
    /// Block height.
    pub height: u32,
    /// Hex (display order) of the previous block this builds on.
    pub prev_blockhash: String,
    /// Network for which this template was generated, e.g.
    /// `"bitcoin"`, `"signet"`, `"regtest"`.
    pub network: String,

    /// Total reward to the coinbase: subsidy plus fees.
    pub coinbase_value_sats: u64,
    /// Block reward by the BIP34 schedule for this height.
    pub subsidy_sats: u64,
    /// Sum of fees the miner collects, computed as
    /// `coinbase_value - subsidy`.
    pub fees_sats: u64,
    /// Configured payout address that would receive the reward.
    pub payout_address: String,
    /// ASCII-renderable bytes of the operator's coinbase scriptSig
    /// vanity message (height push and extranonce stripped). Bytes
    /// outside printable ASCII are replaced with `?`.
    pub coinbase_message: String,

    /// Number of transactions in the template (excluding coinbase).
    pub tx_count: usize,
    /// Total weight of the template in weight units.
    pub weight: u64,
    /// Weight as a percentage of the 4_000_000 limit.
    pub weight_pct: f32,
    /// The transaction paying the highest fee rate, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_fee_tx: Option<TxSummary>,
    /// The single largest transaction by weight, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub biggest_tx: Option<TxSummary>,
    /// Lowest fee rate (sat/vB) that cleared into this template.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fee_floor_sat_per_vb: Option<f64>,
    /// Count of transactions whose output shapes look like Lightning
    /// (P2WSH + small anchor outputs, etc.). Heuristic; can mis-tag.
    pub ln_shaped_count: u32,

    /// Halving era this height belongs to (0 = first 210k blocks).
    pub halving_era: u32,
    /// Distance to the next halving boundary, in blocks.
    pub blocks_until_halving: u32,
    /// Position within the 2016-block retarget window
    /// (`height % 2016`).
    pub retarget_position: u32,
    /// Network target (PoW) as a 32-byte big-endian hex string.
    pub network_target_hex: String,
    /// Signet challenge script hex, when on signet. `None` on
    /// mainnet/testnet/regtest.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signet_challenge_hex: Option<String>,
}

/// One transaction summarised for display.
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
pub struct TxSummary {
    /// Hex (display order) of the txid, possibly truncated for UI.
    pub txid: String,
    /// Fee in sats.
    pub fee_sats: u64,
    /// Virtual size (rounded-up `weight / 4`).
    pub vbytes: u32,
    /// Effective fee rate.
    pub sat_per_vb: f64,
    /// Output-shape heuristic: e.g. `"ln-funding-2of2"`,
    /// `"ln-anchor"`, `"wallet-self-send"`. Best-effort only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shape_hint: Option<String>,
}

/// Serialize an `Option<f64>` so that whole numbers appear without a
/// fractional part (e.g. `2328` instead of `2328.0`).
fn serialize_opt_f64_as_integer_when_whole<S: serde::Serializer>(
    value: &Option<f64>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match value {
        None => serializer.serialize_none(),
        Some(v) if v.fract() == 0.0 && v.is_finite() => serializer.serialize_i64(*v as i64),
        Some(v) => serializer.serialize_f64(*v),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_difficulty_serializes_as_integer() {
        let source = SourceTelemetry {
            difficulty: Some(2048.0),
            ..Default::default()
        };
        let json: serde_json::Value = serde_json::to_value(&source).unwrap();
        assert!(
            json["difficulty"].is_u64(),
            "expected integer, got {}",
            json["difficulty"]
        );
    }

    #[test]
    fn fractional_difficulty_serializes_as_float() {
        let source = SourceTelemetry {
            difficulty: Some(2048.5),
            ..Default::default()
        };
        let json: serde_json::Value = serde_json::to_value(&source).unwrap();
        assert!(
            json["difficulty"].is_f64(),
            "expected float, got {}",
            json["difficulty"]
        );
    }
}
