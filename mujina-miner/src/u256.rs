//! 256-bit unsigned integer arithmetic.
//!
//! Wraps `ruint::aliases::U256` to provide a stable interface. This adapter
//! module exists so we can swap the underlying library or implement our own
//! arithmetic without changing callers.

use ruint::aliases::U256 as Ruint256;
use std::ops::{Div, Mul};

/// A 256-bit unsigned integer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct U256(Ruint256);

impl U256 {
    /// Zero constant.
    pub const ZERO: Self = Self(Ruint256::ZERO);

    /// Create from little-endian bytes.
    pub fn from_le_bytes(bytes: [u8; 32]) -> Self {
        Self(Ruint256::from_le_bytes(bytes))
    }

    /// Convert to little-endian bytes.
    pub fn to_le_bytes(self) -> [u8; 32] {
        self.0.to_le_bytes()
    }

    /// Convert to u64, saturating at u64::MAX.
    pub fn saturating_to_u64(self) -> u64 {
        self.0.saturating_to()
    }
}

impl Div for U256 {
    type Output = Self;

    fn div(self, rhs: Self) -> Self::Output {
        Self(self.0 / rhs.0)
    }
}

impl Div<u64> for U256 {
    type Output = Self;

    fn div(self, rhs: u64) -> Self::Output {
        Self(self.0 / Ruint256::from(rhs))
    }
}

impl Mul<u64> for U256 {
    type Output = Self;

    fn mul(self, rhs: u64) -> Self::Output {
        Self(self.0 * Ruint256::from(rhs))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_division_u256() {
        let a = U256::from_le_bytes({
            let mut bytes = [0u8; 32];
            bytes[0] = 100;
            bytes
        });
        let b = U256::from_le_bytes({
            let mut bytes = [0u8; 32];
            bytes[0] = 10;
            bytes
        });
        let expected = U256::from_le_bytes({
            let mut bytes = [0u8; 32];
            bytes[0] = 10;
            bytes
        });
        assert_eq!(a / b, expected);
    }

    #[test]
    fn test_division_u64() {
        let a = U256::from_le_bytes({
            let mut bytes = [0u8; 32];
            bytes[0] = 100;
            bytes
        });
        let expected = U256::from_le_bytes({
            let mut bytes = [0u8; 32];
            bytes[0] = 10;
            bytes
        });
        assert_eq!(a / 10u64, expected);
    }

    #[test]
    fn test_large_division() {
        // Large value / 1 = same value
        let large = U256::from_le_bytes([0xff; 32]);
        let one = U256::from_le_bytes({
            let mut bytes = [0u8; 32];
            bytes[0] = 1;
            bytes
        });
        assert_eq!(large / one, large);
    }
}
