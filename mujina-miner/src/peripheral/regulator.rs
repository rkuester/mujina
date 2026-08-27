//! Voltage regulator consumer interface.
//!
//! The trait carries the contract between a board and the code that
//! consumes a rail. Regulator chip drivers in this layer implement
//! it; a board picks the implementation and hands it to the
//! consumer.

use std::ops::RangeInclusive;

use anyhow::Result;
use async_trait::async_trait;

use crate::systree::{Cursor, Report, Setting, within};
use crate::types::{Temperature, Voltage};

/// Voltage regulator control for a consumer of one rail.
///
/// A consumer handle: the board is the provider and sets the
/// limits; the consumer operates the rail through this handle.
/// Voltages here are set points, what the regulator is programmed
/// to deliver, never measurements. The measured rail voltage is
/// telemetry and lives outside this trait.
#[async_trait]
pub trait VoltageRegulator: Send + Sync {
    /// Enable the regulator output.
    async fn enable(&mut self) -> Result<()>;

    /// Disable the regulator output.
    async fn disable(&mut self) -> Result<()>;

    /// Whether the regulator output is enabled.
    ///
    /// Reports the switch state, not the rail: a disabled output
    /// reads false while the rail decays, and an enabled output
    /// reads true even if the rail has collapsed.
    async fn is_enabled(&mut self) -> Result<bool>;

    /// Set the output voltage set point.
    async fn set_voltage(&mut self, voltage: Voltage) -> Result<()>;

    /// The output voltage set point.
    ///
    /// The set point keeps its value while the output is disabled.
    async fn get_voltage(&mut self) -> Result<Voltage>;
}

/// What a voltage regulator holds in the tree.
///
/// A regulator's readings and its setpoint sit together, so the
/// voltage a client asks for is beside the voltage the rail
/// actually reaches.
pub struct RegulatorAttributes {
    /// The voltage at the regulator's input.
    pub input_voltage: Report<Option<Voltage>>,

    /// The voltage the rail actually reaches.
    pub voltage: Report<Option<Voltage>>,

    /// The current the rail delivers, in amperes.
    pub current: Report<Option<f32>>,

    /// The power the rail delivers, in watts.
    pub power: Report<Option<f32>>,

    /// The regulator's own temperature.
    pub temperature: Report<Option<Temperature>>,

    /// The voltage a client asks the rail to reach.
    ///
    /// Unset means the daemon decides. The board holds the limits,
    /// so the range comes from whoever attaches this.
    pub target_voltage: Setting<Option<Voltage>>,
}

impl RegulatorAttributes {
    /// Attaches a regulator's attributes under a cursor.
    ///
    /// The board passes the range its rail may be asked for, since
    /// the board is the provider and sets the limits.
    pub fn attach(sys: &Cursor, limits: RangeInclusive<Voltage>) -> Self {
        Self {
            input_voltage: sys.report("input_voltage", None),
            voltage: sys.report("voltage", None),
            current: sys.report("current_a", None),
            power: sys.report("power_w", None),
            temperature: sys.report("temperature", None),
            target_voltage: sys.setting_checked("target_voltage", None, within(limits)),
        }
    }
}
