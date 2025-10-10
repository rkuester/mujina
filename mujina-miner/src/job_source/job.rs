//! Mining job and share types.

use bitcoin::block::Version;
use bitcoin::hash_types::{BlockHash, TxMerkleNode};
use bitcoin::pow::{CompactTarget, Target};

use crate::types::Extranonce2;

/// Represents a mining job from any source.
///
/// All job sources (pool, solo, dummy) provide coinbase components and extranonce
/// parameters. The merkle root is calculated dynamically as extranonce2 is rolled.
#[derive(Debug, Clone)]
pub struct MiningJob {
    /// Unique identifier for this job
    pub job_id: String,

    /// Previous block hash
    pub prev_blockhash: BlockHash,

    /// Block version
    pub version: Version,

    /// Encoded difficulty target
    pub bits: CompactTarget,

    /// Block timestamp
    pub time: u32,

    /// First part of coinbase transaction (before extranonces)
    pub coinbase1: Vec<u8>,

    /// Second part of coinbase transaction (after extranonces)
    pub coinbase2: Vec<u8>,

    /// Merkle branches for building block header
    /// - Pool: pre-computed by pool from their transaction selection
    /// - Solo: computed from mempool transactions via getblocktemplate
    /// - Dummy: empty (coinbase only) or synthetic transactions
    pub merkle_branches: Vec<TxMerkleNode>,

    /// Extranonce1 value (from pool or synthetic for solo/dummy)
    pub extranonce1: Vec<u8>,

    /// Template extranonce2 with correct size, initialized to zero
    pub extranonce2_template: Extranonce2,

    /// Clean jobs flag - if true, abort current work immediately
    pub clean_jobs: bool,

    /// Mask for version rolling (BIP320 AsicBoost)
    pub version_mask: Option<u32>,
}

impl MiningJob {
    /// Calculate the total extranonce2 search space size.
    ///
    /// Returns the number of unique extranonce2 values available.
    /// For example, 4 bytes = 2^32 = 4,294,967,296 combinations.
    pub fn extranonce2_space(&self) -> u64 {
        self.extranonce2_template.search_space()
    }

    /// Get the target difficulty as a Target type.
    pub fn target(&self) -> Target {
        Target::from(self.bits)
    }
}

/// Represents a share submission (solved work).
#[derive(Debug, Clone)]
pub struct Share {
    /// Job ID this share is for
    pub job_id: String,

    /// Nonce that solves the work
    pub nonce: u32,

    /// Extra nonce 2 (for pool mining)
    pub extranonce2: Option<Extranonce2>,

    /// nTime value (if rolled)
    pub ntime: Option<u32>,

    /// Version bits (if rolled)
    pub version_bits: Option<u16>,
}
