//! Voltage type for representing rail and supply voltages.
//!
//! Stores voltage internally as microvolts (i64) for precision,
//! following the pattern of `std::time::Duration`. Signed, so
//! negative voltages represent correctly. Convert on access via the
//! unit-specific methods.

use std::fmt;

use serde_json::Value;

use crate::systree::{AttributeValue, SettingValue, ValueError};

/// Voltage in microvolts.
///
/// A unit-aware voltage type. Stores microvolts internally for
/// precision; convert on access. Signed: negative voltages are
/// valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Voltage {
    uv: i64,
}

impl Voltage {
    /// Creates a voltage from microvolts.
    pub const fn from_uv(uv: i64) -> Self {
        Self { uv }
    }

    /// Creates a voltage from millivolts.
    pub const fn from_mv(mv: i32) -> Self {
        Self {
            uv: mv as i64 * 1_000,
        }
    }

    /// Creates a voltage from volts.
    pub const fn from_volts(volts: f32) -> Self {
        Self {
            uv: (volts * 1_000_000.0) as i64,
        }
    }

    /// Returns the voltage in microvolts.
    pub const fn uv(&self) -> i64 {
        self.uv
    }

    /// Returns the voltage in millivolts.
    pub const fn mv(&self) -> i32 {
        (self.uv / 1_000) as i32
    }

    /// Returns the voltage in volts.
    pub const fn volts(&self) -> f32 {
        self.uv as f32 / 1_000_000.0
    }
}

/// Renders in volts, so a name takes the `_v` suffix.
impl AttributeValue for Voltage {
    const UNIT: Option<&'static str> = Some("v");

    fn to_json(&self) -> Value {
        Value::from(self.volts())
    }
}

impl SettingValue for Voltage {
    fn from_json(value: &Value) -> Result<Self, ValueError> {
        match value.as_f64() {
            Some(volts) => Ok(Self::from_volts(volts as f32)),
            None => Err(ValueError::WrongType("a voltage in volts")),
        }
    }
}

impl fmt::Display for Voltage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.3} V", self.volts())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_uv() {
        let v = Voltage::from_uv(1_150_000);
        assert_eq!(v.uv(), 1_150_000);
        assert_eq!(v.mv(), 1_150);
        assert_eq!(v.volts(), 1.15);
    }

    #[test]
    fn from_mv() {
        let v = Voltage::from_mv(1_150);
        assert_eq!(v.uv(), 1_150_000);
        assert_eq!(v.mv(), 1_150);
        assert_eq!(v.volts(), 1.15);
    }

    #[test]
    fn from_volts() {
        let v = Voltage::from_volts(1.15);
        assert_eq!(v.uv(), 1_150_000);
        assert_eq!(v.mv(), 1_150);
        assert_eq!(v.volts(), 1.15);
    }

    #[test]
    fn negative() {
        let v = Voltage::from_mv(-12_000);
        assert_eq!(v.uv(), -12_000_000);
        assert_eq!(v.mv(), -12_000);
        assert_eq!(v.volts(), -12.0);
        assert!(v < Voltage::default());
    }

    #[test]
    fn ordering() {
        let negative = Voltage::from_volts(-12.0);
        let low = Voltage::from_volts(0.9);
        let high = Voltage::from_volts(1.25);
        assert!(negative < low);
        assert!(low < high);
        assert!(Voltage::from_mv(-100) < Voltage::from_mv(-99));
    }

    #[test]
    fn default_is_zero() {
        let v = Voltage::default();
        assert_eq!(v.uv(), 0);
    }
}
