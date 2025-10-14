//! Extranonce2 type for Bitcoin mining.
//!
//! In Bitcoin mining, the extranonce2 field is the miner's primary mechanism for
//! generating unique work across the search space. This module provides a specialized
//! counter type that manages the complexities of extranonce2 handling:
//!
//! Mining pools allocate a specific byte size for extranonce2 (typically 4-8 bytes),
//! which determines how many unique coinbase transactions a miner can generate before
//! needing new work. The `Extranonce2` type abstracts this variable-sized counter,
//! providing atomic increment operations with wraparound detection, critical for
//! knowing when available work has been exhausted.
//!
//! The type handles the serialization details required by the Stratum protocol and
//! coinbase transaction construction, where the extranonce2 must be inserted as
//! little-endian bytes between extranonce1 and the coinbase suffix. This ensures
//! proper merkle root calculation for each unique block candidate.

use std::fmt;

use thiserror::Error;

/// Extranonce2 value used in mining to generate unique coinbase transactions.
///
/// Stored as a u64 internally but serializes to a variable number of bytes
/// (1-8) as required by the mining protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Extranonce2 {
    value: u64,
    size: u8,
}

/// Errors that can occur when creating Extranonce2 values.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum Extranonce2Error {
    #[error("Invalid extranonce2 size: {0} (must be 1-8 bytes)")]
    InvalidSize(u8),
}

impl Extranonce2 {
    /// Create a new extranonce2 with the specified size in bytes, initialized to zero.
    pub fn new(size_bytes: u8) -> Result<Self, Extranonce2Error> {
        if size_bytes == 0 || size_bytes > 8 {
            return Err(Extranonce2Error::InvalidSize(size_bytes));
        }
        Ok(Self {
            value: 0,
            size: size_bytes,
        })
    }

    /// Increment the value, wrapping on overflow.
    ///
    /// Returns `false` if the value wrapped back to zero (exhausted search space).
    pub fn increment(&mut self) -> bool {
        let max = self.max_value();
        if self.value < max {
            self.value += 1;
            true
        } else {
            self.value = 0;
            false
        }
    }

    /// Get the maximum value for this extranonce2's size.
    pub fn max_value(&self) -> u64 {
        if self.size >= 8 {
            u64::MAX
        } else {
            (1u64 << (self.size * 8)) - 1
        }
    }

    /// Calculate the total search space (number of possible values).
    pub fn search_space(&self) -> u64 {
        if self.size < 8 {
            1u64 << (self.size * 8)
        } else {
            u64::MAX
        }
    }

    /// Extend a vector with the serialized bytes of this extranonce2.
    pub fn extend_vec(&self, vec: &mut Vec<u8>) {
        vec.extend_from_slice(&self.value.to_le_bytes()[..self.size as usize]);
    }
}

impl From<Extranonce2> for Vec<u8> {
    /// Convert to little-endian bytes for inclusion in coinbase transaction.
    fn from(ext: Extranonce2) -> Vec<u8> {
        ext.value.to_le_bytes()[..ext.size as usize].to_vec()
    }
}

impl fmt::Display for Extranonce2 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:0width$x}", self.value, width = self.size as usize * 2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new() {
        let ext = Extranonce2::new(4).unwrap();

        // Test range
        assert_eq!(ext.max_value(), 0xffffffff);
        assert_eq!(ext.search_space(), 1u64 << 32); // 2^32 possible values

        // Test initial serialization
        let bytes: Vec<u8> = ext.into();
        assert_eq!(bytes, vec![0, 0, 0, 0]); // Should be 4 zero bytes

        // Test display format
        assert_eq!(format!("{}", ext), "00000000");
    }

    #[test]
    fn test_new_errors() {
        assert!(matches!(
            Extranonce2::new(0),
            Err(Extranonce2Error::InvalidSize(0))
        ));
        assert!(matches!(
            Extranonce2::new(9),
            Err(Extranonce2Error::InvalidSize(9))
        ));
    }

    #[test]
    fn test_increment() {
        let mut ext = Extranonce2::new(1).unwrap();

        // Should increment normally
        assert!(ext.increment());
        assert_eq!(Vec::<u8>::from(ext), vec![1]);

        // Increment up to near the end
        for _ in 0..253 {
            assert!(ext.increment());
        }
        assert_eq!(Vec::<u8>::from(ext), vec![254]);

        // One more increment
        assert!(ext.increment());
        assert_eq!(Vec::<u8>::from(ext), vec![255]);

        // Should wrap and return false
        assert!(!ext.increment());
        assert_eq!(Vec::<u8>::from(ext), vec![0]);
    }

    #[test]
    fn test_max_value() {
        assert_eq!(Extranonce2::new(1).unwrap().max_value(), 0xff);
        assert_eq!(Extranonce2::new(2).unwrap().max_value(), 0xffff);
        assert_eq!(Extranonce2::new(4).unwrap().max_value(), 0xffff_ffff);
        assert_eq!(Extranonce2::new(6).unwrap().max_value(), 0xffff_ffff_ffff);
        assert_eq!(Extranonce2::new(8).unwrap().max_value(), u64::MAX);
    }

    #[test]
    fn test_search_space() {
        assert_eq!(Extranonce2::new(1).unwrap().search_space(), 256);
        assert_eq!(Extranonce2::new(2).unwrap().search_space(), 65536);
        assert_eq!(Extranonce2::new(4).unwrap().search_space(), 1u64 << 32);
        assert_eq!(Extranonce2::new(6).unwrap().search_space(), 1u64 << (6 * 8));
        assert_eq!(Extranonce2::new(8).unwrap().search_space(), u64::MAX);
    }

    #[test]
    fn test_to_bytes() {
        // Test zero value
        let ext = Extranonce2::new(2).unwrap();
        assert_eq!(Vec::<u8>::from(ext), vec![0, 0]);

        // Test after incrementing
        let mut ext = Extranonce2::new(2).unwrap();
        for _ in 0..0x1234 {
            ext.increment();
        }
        let bytes: Vec<u8> = ext.into();
        assert_eq!(bytes, vec![0x34, 0x12]); // Little-endian

        // Test single byte
        let mut ext = Extranonce2::new(1).unwrap();
        for _ in 0..0xab {
            ext.increment();
        }
        assert_eq!(Vec::<u8>::from(ext), vec![0xab]);
    }

    #[test]
    fn test_display() {
        // Test zero value
        let ext = Extranonce2::new(4).unwrap();
        assert_eq!(format!("{}", ext), "00000000");

        // Test after incrementing
        let mut ext = Extranonce2::new(4).unwrap();
        for _ in 0..0x1234 {
            ext.increment();
        }
        assert_eq!(format!("{}", ext), "00001234");

        // Test 2-byte display
        let mut ext = Extranonce2::new(2).unwrap();
        for _ in 0..0xab {
            ext.increment();
        }
        assert_eq!(format!("{}", ext), "00ab");
    }
}
