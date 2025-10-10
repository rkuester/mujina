//! Test data from real Bitcoin blocks.
//!
//! This module provides well-known Bitcoin blocks testing, with all their
//! components available in various formats.

/// Block 881,423
///
/// On January 30, 2025, the 256 Foundation held its first "Telehash" fundraiser,
/// directing over 1.12 EH/s from approximately 350 participants---mostly home and
/// small-scale miners aligned with the Foundation's mission to make Bitcoin
/// mining accessible through open-source tools---to a self-hosted mining pool.
/// During the live-streamed event, against incredible odds, they actually solved
/// this block, raising 3.146 BTC (~$330,300 at the time) to bootstrap several
/// open-source Bitcoin mining projects including Mujina.
///
/// This block serves as our primary test data for validating mining job generation
/// and header construction.
pub mod block_881423 {
    use std::str::FromStr;
    use std::sync::LazyLock;

    use bitcoin::block::{Header as BlockHeader, Version};
    use bitcoin::hash_types::{BlockHash, TxMerkleNode};
    use bitcoin::hashes::Hash;
    use bitcoin::pow::CompactTarget;

    // Ground truth field values: These typed constants represent the authoritative
    // field values from block 881,423, parsed from the blockchain. They provide
    // the same data as HEADER_BYTES below, but in rust-bitcoin types.
    //
    // LazyLock is used for rust-bitcoin types because their constructors
    // (from_consensus, from_byte_array, etc.) are not const fn and cannot be
    // evaluated at compile time. LazyLock provides thread-safe lazy
    // initialization on first access.

    /// Block version field
    pub static VERSION: LazyLock<Version> =
        LazyLock::new(|| Version::from_consensus(0x2e596000_u32 as i32));

    /// Previous block hash
    pub static PREV_BLOCKHASH: LazyLock<BlockHash> = LazyLock::new(|| {
        BlockHash::from_byte_array([
            0xe3, 0xc9, 0x6a, 0x8a, 0x70, 0xca, 0x15, 0xfe, 0x21, 0x5e, 0x99, 0xf8, 0x4a, 0xb6,
            0x4c, 0x43, 0xe1, 0xb5, 0xd2, 0xc4, 0x3b, 0x54, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00,
        ])
    });

    /// Merkle root
    pub static MERKLE_ROOT: LazyLock<TxMerkleNode> = LazyLock::new(|| {
        TxMerkleNode::from_byte_array([
            0x76, 0xf6, 0x3a, 0x35, 0xf1, 0xb8, 0xb5, 0x01, 0x6d, 0x3e, 0xb0, 0xcc, 0xe2, 0xee,
            0xbb, 0xdf, 0x58, 0xbf, 0x8f, 0xbe, 0xa4, 0xe8, 0x70, 0xc8, 0xf7, 0x70, 0x34, 0x6b,
            0xdf, 0xcf, 0x62, 0x2d,
        ])
    });

    /// Block timestamp
    pub const TIME: u32 = 0x679ac169;

    /// Difficulty target (nBits field)
    pub static BITS: LazyLock<CompactTarget> =
        LazyLock::new(|| CompactTarget::from_consensus(0x17029a8a));

    /// Block nonce
    pub const NONCE: u32 = 0xff05fb02;

    // Ground truth wire format: the complete 80-byte header as it appears in the
    // blockchain. The typed field constants above are parsed from these bytes.
    // Consistency tests ensure both representations stay synchronized.

    /// The complete 80-byte block header in binary form.
    pub const HEADER_BYTES: [u8; 80] = [
        // Version (all fields little-endian)
        0x00, 0x60, 0x59, 0x2e, // Previous block hash
        0xe3, 0xc9, 0x6a, 0x8a, 0x70, 0xca, 0x15, 0xfe, 0x21, 0x5e, 0x99, 0xf8, 0x4a, 0xb6, 0x4c,
        0x43, 0xe1, 0xb5, 0xd2, 0xc4, 0x3b, 0x54, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, // Merkle root
        0x76, 0xf6, 0x3a, 0x35, 0xf1, 0xb8, 0xb5, 0x01, 0x6d, 0x3e, 0xb0, 0xcc, 0xe2, 0xee, 0xbb,
        0xdf, 0x58, 0xbf, 0x8f, 0xbe, 0xa4, 0xe8, 0x70, 0xc8, 0xf7, 0x70, 0x34, 0x6b, 0xdf, 0xcf,
        0x62, 0x2d, // Timestamp
        0x69, 0xc1, 0x9a, 0x67, // Bits
        0x8a, 0x9a, 0x02, 0x17, // Nonce
        0x02, 0xfb, 0x05, 0xff,
    ];

    // Ground truth block hash from the blockchain. Tests verify that both
    // HEADER_BYTES and the typed field constants produce this hash.

    /// Block hash
    pub const BLOCK_HASH: LazyLock<BlockHash> = LazyLock::new(|| {
        BlockHash::from_str("0000000000000000000269d52c24ea451225613aab095d90d771d4e29aa96cdd")
            .unwrap()
    });

    /// Complete block header constructed from ground truth field values
    pub static HEADER: LazyLock<BlockHeader> = LazyLock::new(|| BlockHeader {
        version: *VERSION,
        prev_blockhash: *PREV_BLOCKHASH,
        merkle_root: *MERKLE_ROOT,
        time: TIME,
        bits: *BITS,
        nonce: NONCE,
    });

    /// Get version bytes
    pub fn version_bytes() -> [u8; 4] {
        [
            HEADER_BYTES[0],
            HEADER_BYTES[1],
            HEADER_BYTES[2],
            HEADER_BYTES[3],
        ]
    }

    /// Get previous block hash bytes
    pub fn prev_hash_bytes() -> [u8; 32] {
        let mut bytes = [0u8; 32];
        for i in 0..32 {
            bytes[i] = HEADER_BYTES[4 + i];
        }
        bytes
    }

    /// Get merkle root bytes
    pub fn merkle_root_bytes() -> [u8; 32] {
        let mut bytes = [0u8; 32];
        for i in 0..32 {
            bytes[i] = HEADER_BYTES[36 + i];
        }
        bytes
    }

    /// Get timestamp bytes
    pub fn time_bytes() -> [u8; 4] {
        [
            HEADER_BYTES[68],
            HEADER_BYTES[69],
            HEADER_BYTES[70],
            HEADER_BYTES[71],
        ]
    }

    /// Get bits bytes
    pub fn bits_bytes() -> [u8; 4] {
        [
            HEADER_BYTES[72],
            HEADER_BYTES[73],
            HEADER_BYTES[74],
            HEADER_BYTES[75],
        ]
    }

    /// Get nonce bytes
    pub fn nonce_bytes() -> [u8; 4] {
        [
            HEADER_BYTES[76],
            HEADER_BYTES[77],
            HEADER_BYTES[78],
            HEADER_BYTES[79],
        ]
    }

    // Components for reconstructing the mining job
    // These would have been the inputs to create this block

    /// Example coinbase transaction prefix (before extranonces)
    /// This is what a pool would send as coinbase1
    /// For now, this is a placeholder - we'd need the actual coinbase transaction
    pub const COINBASE1: &[u8] = &[
        0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff,
    ];

    /// Example coinbase transaction suffix (after extranonces)
    /// This is what a pool would send as coinbase2
    pub const COINBASE2: &[u8] = &[
        0xff, 0xff, 0xff, 0xff, 0x01, 0x00, 0xf2, 0x05, 0x2a, 0x01, 0x00, 0x00, 0x00, 0x43, 0x41,
        0x04,
    ];

    /// Example extranonce1 (from pool)
    pub const EXTRANONCE1: &[u8] = &[0x08, 0x00, 0x00, 0x02];

    /// Example extranonce2 that was used
    pub const EXTRANONCE2: &[u8] = &[0x00, 0x00, 0x00, 0x00];

    /// Merkle branches for this block (if any transactions besides coinbase)
    pub const MERKLE_BRANCHES: &[&[u8]] = &[
        // Empty for now - would need actual transaction hashes
    ];

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn test_version_consistency() {
            assert_eq!(
                Version::from_consensus(i32::from_le_bytes(version_bytes())),
                *VERSION
            );
        }

        #[test]
        fn test_prev_blockhash_consistency() {
            assert_eq!(
                BlockHash::from_byte_array(prev_hash_bytes()),
                *PREV_BLOCKHASH
            );
        }

        #[test]
        fn test_merkle_root_consistency() {
            assert_eq!(
                TxMerkleNode::from_byte_array(merkle_root_bytes()),
                *MERKLE_ROOT
            );
        }

        #[test]
        fn test_time_consistency() {
            assert_eq!(u32::from_le_bytes(time_bytes()), TIME);
        }

        #[test]
        fn test_bits_consistency() {
            assert_eq!(
                CompactTarget::from_consensus(u32::from_le_bytes(bits_bytes())),
                *BITS
            );
        }

        #[test]
        fn test_nonce_consistency() {
            assert_eq!(u32::from_le_bytes(nonce_bytes()), NONCE);
        }

        #[test]
        fn test_block_hash_consistency() {
            // Check hash from HEADER_BYTES against ground truth
            let hash_from_bytes = BlockHash::hash(&HEADER_BYTES);
            assert_eq!(hash_from_bytes, *BLOCK_HASH, "HEADER_BYTES hash mismatch");

            // Check hash from HEADER against ground truth
            let hash_from_header = HEADER.block_hash();
            assert_eq!(hash_from_header, *BLOCK_HASH, "HEADER hash mismatch");
        }
    }
}
