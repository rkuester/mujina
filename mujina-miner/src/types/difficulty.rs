//! Integer difficulty type.

use crate::u256::U256;
use bitcoin::hash_types::BlockHash;
use bitcoin::hashes::Hash;
use bitcoin::pow::Target;
use std::fmt;

/// Integer mining difficulty.
///
/// Represents difficulty as an integer multiplier. Used for:
/// - Stratum protocol (pools communicate difficulty as integers)
/// - Logging and display (human-readable values)
/// - Configuration and statistics
///
/// For actual share validation, convert to `Target` via `to_target()`. Bitcoin
/// blocks use `CompactTarget` in their headers, while Stratum uses integer
/// difficulty values for pool-miner communication.
///
/// In Bitcoin's proof-of-work, a hash is valid if it's numerically less than
/// or equal to a target value. Difficulty and target have an inverse
/// relationship:
/// ```text
/// target = MAX_TARGET / difficulty
/// ```
///
/// - Difficulty 1: target = MAX_TARGET (largest target, easiest)
/// - Difficulty 1000: target = MAX_TARGET / 1000 (smaller target, harder)
///
/// Higher difficulty produces a smaller target, meaning fewer hash values
/// qualify as valid, requiring more hashing attempts on average to find
/// a valid share.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Difficulty(u64);

impl Difficulty {
    /// Maximum difficulty (used when hash is zero or impossibly small).
    pub const MAX: Self = Self(u64::MAX);

    /// Create a new difficulty value.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Calculate difficulty from a target.
    ///
    /// Uses the formula: difficulty = MAX_TARGET / target
    ///
    /// This is the inverse of `to_target()`.
    pub fn from_target(target: Target) -> Self {
        let target_u256 = U256::from(target);
        if target_u256 == U256::ZERO {
            return Self(u64::MAX);
        }

        let max_target = U256::from(Target::MAX);
        let difficulty = max_target / target_u256;
        Self(difficulty.saturating_to_u64())
    }

    /// Calculate difficulty from a block hash.
    ///
    /// Uses the formula: difficulty = MAX_TARGET / hash
    ///
    /// This is useful for determining what difficulty a found share represents,
    /// primarily for logging and statistics.
    pub fn from_hash(hash: &BlockHash) -> Self {
        let hash_u256 = U256::from_le_bytes(*hash.as_byte_array());
        if hash_u256 == U256::ZERO {
            return Self(u64::MAX);
        }

        let max_target = U256::from(Target::MAX);
        let difficulty = max_target / hash_u256;
        Self(difficulty.saturating_to_u64())
    }

    /// Convert difficulty to Target.
    ///
    /// Use the returned `Target` for actual share validation (comparing against
    /// block hashes). This conversion uses exact 256-bit division:
    /// `target = MAX_TARGET / difficulty`
    ///
    /// This is the inverse of `target.difficulty_float()`.
    pub fn to_target(self) -> Target {
        if self.0 == 0 || self.0 == 1 {
            return Target::MAX;
        }

        let max_target_u256 = U256::from(Target::MAX);
        let target_u256 = max_target_u256 / self.0;
        Target::from(target_u256)
    }
}

impl From<Difficulty> for u64 {
    fn from(diff: Difficulty) -> Self {
        diff.0
    }
}

impl fmt::Display for Difficulty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = self.0 as f64;

        // Format with SI suffixes (K, M, G, T, P)
        let (scaled, suffix) = if value >= 1e15 {
            (value / 1e15, "P")
        } else if value >= 1e12 {
            (value / 1e12, "T")
        } else if value >= 1e9 {
            (value / 1e9, "G")
        } else if value >= 1e6 {
            (value / 1e6, "M")
        } else if value >= 1e3 {
            (value / 1e3, "K")
        } else {
            (value, "")
        };

        // Round to appropriate precision
        if scaled >= 100.0 {
            write!(f, "{:.0}{}", scaled, suffix) // "112T"
        } else if scaled >= 10.0 {
            write!(f, "{:.1}{}", scaled, suffix) // "11.2T"
        } else {
            write!(f, "{:.2}{}", scaled, suffix) // "1.12T"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::hashes::Hash;

    #[test]
    fn test_difficulty_to_u64() {
        let diff = Difficulty::new(1024);
        assert_eq!(u64::from(diff), 1024);
    }

    #[test]
    fn test_difficulty_to_target() {
        // Difficulty 1 should equal MAX target
        let diff = Difficulty::new(1);
        assert_eq!(diff.to_target(), Target::MAX);

        // Difficulty 0 treated as 1 (edge case)
        let diff = Difficulty::new(0);
        assert_eq!(diff.to_target(), Target::MAX);

        // Higher difficulty should produce smaller target
        let diff_low = Difficulty::new(100);
        let diff_high = Difficulty::new(1000);
        assert!(diff_high.to_target() < diff_low.to_target());
    }

    #[test]
    fn test_difficulty_from_target() {
        // Target::MAX gives difficulty 1
        assert_eq!(Difficulty::from_target(Target::MAX), Difficulty::new(1));

        // Round-trip: difficulty -> target -> difficulty
        let original = Difficulty::new(1024);
        let recovered = Difficulty::from_target(original.to_target());
        assert_eq!(recovered, original);

        // Larger difficulty round-trip
        let original = Difficulty::new(1_000_000);
        let recovered = Difficulty::from_target(original.to_target());
        assert_eq!(recovered, original);
    }

    #[test]
    fn test_difficulty_display() {
        // High difficulty (petahash range)
        let diff = Difficulty::new(1_500_000_000_000_000);
        assert_eq!(diff.to_string(), "1.50P");

        // Terahash range
        let diff = Difficulty::new(112_700_000_000_000);
        assert_eq!(diff.to_string(), "113T");

        let diff = Difficulty::new(11_200_000_000_000);
        assert_eq!(diff.to_string(), "11.2T");

        let diff = Difficulty::new(1_120_000_000_000);
        assert_eq!(diff.to_string(), "1.12T");

        // Gigahash range
        let diff = Difficulty::new(500_000_000_000);
        assert_eq!(diff.to_string(), "500G");

        // Megahash range
        let diff = Difficulty::new(1_500_000);
        assert_eq!(diff.to_string(), "1.50M");

        // Small values
        let diff = Difficulty::new(500);
        assert_eq!(diff.to_string(), "500");
    }

    #[test]
    fn test_difficulty_from_hash() {
        // Target::MAX gives difficulty 1
        let hash = BlockHash::from_byte_array(Target::MAX.to_le_bytes());
        assert_eq!(Difficulty::from_hash(&hash), Difficulty::new(1));

        // Half of Target::MAX gives difficulty 2
        let mut bytes = Target::MAX.to_le_bytes();
        // Shift right by 1 bit (divide by 2)
        let mut carry = 0u8;
        for byte in bytes.iter_mut().rev() {
            let new_carry = *byte & 1;
            *byte = (*byte >> 1) | (carry << 7);
            carry = new_carry;
        }
        let hash = BlockHash::from_byte_array(bytes);
        assert_eq!(Difficulty::from_hash(&hash), Difficulty::new(2));

        // Very small hash gives high difficulty
        let mut bytes = [0u8; 32];
        bytes[0] = 1; // Smallest non-zero LE value
        let hash = BlockHash::from_byte_array(bytes);
        assert!(Difficulty::from_hash(&hash) > Difficulty::new(1_000_000));

        // Zero hash saturates to MAX
        let hash = BlockHash::from_byte_array([0u8; 32]);
        assert_eq!(Difficulty::from_hash(&hash), Difficulty::MAX);
    }
}
