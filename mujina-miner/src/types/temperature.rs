//! Temperature measurement type.

mod conversions;

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::systree::{AttributeValue, SettingValue, ValueError};

/// Temperature in degrees Celsius.
///
/// A newtype preventing bare floats from being passed where a
/// temperature is expected. This catches two classes of mistakes
/// at compile time: using the wrong quantity (e.g., a voltage
/// where a temperature belongs) and using the wrong unit.
/// Constructors like from_celsius make the input unit explicit;
/// additional constructors (from_fahrenheit, etc.) can convert
/// on the way in.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Temperature(f32);

impl Temperature {
    /// Construct from a value in degrees Celsius.
    pub fn from_celsius(degrees: f32) -> Self {
        Self(degrees)
    }

    /// Return the temperature in degrees Celsius.
    pub fn as_degrees_c(self) -> f32 {
        self.0
    }
}

impl fmt::Display for Temperature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.1} C", self.0)
    }
}

/// Renders in degrees Celsius, so a name takes the `_c` suffix.
impl AttributeValue for Temperature {
    const UNIT: Option<&'static str> = Some("c");

    fn to_json(&self) -> Value {
        Value::from(self.as_degrees_c())
    }
}

impl SettingValue for Temperature {
    fn from_json(value: &Value) -> Result<Self, ValueError> {
        match value.as_f64() {
            Some(degrees) => Ok(Self::from_celsius(degrees as f32)),
            None => Err(ValueError::WrongType("a temperature in degrees Celsius")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let t = Temperature::from_celsius(42.5);
        assert_eq!(t.as_degrees_c(), 42.5);
    }

    #[test]
    fn display() {
        let t = Temperature::from_celsius(42.5);
        assert_eq!(t.to_string(), "42.5 C");
    }

    #[test]
    fn ordering() {
        let cold = Temperature::from_celsius(10.0);
        let hot = Temperature::from_celsius(80.0);
        assert!(cold < hot);
    }

    #[test]
    fn negative() {
        let t = Temperature::from_celsius(-40.0);
        assert_eq!(t.as_degrees_c(), -40.0);
        assert_eq!(t.to_string(), "-40.0 C");
    }
}
