//! Board-supplied hardware interfaces for a BM13xx hash thread.
//!
//! A board assembles these and hands them to the thread it creates.
//! The thread operates them; the board decides what hardware sits
//! behind each one.

use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::watch;

use crate::peripheral::regulator::VoltageRegulator;

/// The chips' reset line.
///
/// Hash threads hold the chips in reset until bring-up and assert
/// reset again at shutdown. What drives the line (a GPIO, a
/// management protocol command, etc.) is the board's concern.
#[async_trait]
pub trait ResetLine: Send + Sync {
    /// Asserts reset, stopping the chips.
    async fn assert(&mut self) -> Result<()>;

    /// Releases reset, letting the chips run.
    async fn release(&mut self) -> Result<()>;
}

/// Hardware interfaces provided by the board to the hash thread.
///
/// Every board supplies every interface. A board whose hardware
/// lacks a control (a fixed rail, no host-driven reset) supplies an
/// implementation declaring that, such as a regulator whose disable
/// is a no-op, rather than omitting the interface.
pub struct BoardPeripherals {
    /// The chips' reset line
    pub reset_line: Box<dyn ResetLine>,

    /// Voltage regulator control
    pub voltage_regulator: Box<dyn VoltageRegulator>,

    /// The most of its full power the board lets the thread run at,
    /// 0.0 to 1.0. The board lowers it as its chips approach their
    /// limit; the thread holds the smaller of this and what it was
    /// asked. A board with no die sensor supplies
    /// [`unlimited_power`].
    pub power_ceiling: watch::Receiver<f32>,
}

/// A power ceiling that stays at full. The sender is dropped, so
/// the receiver never changes and the thread never waits on it.
pub fn unlimited_power() -> watch::Receiver<f32> {
    let (_, rx) = watch::channel(1.0);
    rx
}
