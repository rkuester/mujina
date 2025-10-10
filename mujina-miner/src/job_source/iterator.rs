//! Iterator for generating block headers from mining jobs.

use bitcoin::block::Header as BlockHeader;
use bitcoin::hash_types::TxMerkleNode;
use bitcoin::hashes::{sha256d, Hash};

use super::MiningJob;
use crate::types::Extranonce2;

/// Represents a single work unit from a mining job.
pub struct JobWork {
    /// The block header to mine
    pub header: BlockHeader,
    /// The extranonce2 value used for this header
    pub extranonce2: Extranonce2,
    /// Reference to the original job ID
    pub job_id: String,
}

/// Iterator for generating block headers from a mining job by rolling extranonce2.
///
/// This iterator takes a reference to a [`MiningJob`] and generates [`BlockHeader`]s
/// by iterating through possible extranonce2 values. For each extranonce2, it:
/// 1. Builds the complete coinbase transaction
/// 2. Calculates the coinbase transaction hash
/// 3. Builds the merkle root from coinbase hash and merkle branches
/// 4. Creates a block header with the calculated merkle root
///
/// # Example
///
/// ```ignore
/// let job = source.get_job().await?;
///
/// for work in JobWorkIterator::new(&job) {
///     send_to_asic(work.header);
///     // extranonce2 is tracked in work.extranonce2
/// }
/// ```
pub struct JobWorkIterator<'a> {
    /// The mining job being iterated over
    job: &'a MiningJob,
    /// Current extranonce2 value
    current_extranonce2: Extranonce2,
    /// Whether we've exhausted all extranonce2 values
    exhausted: bool,
}

impl<'a> JobWorkIterator<'a> {
    /// Create a new iterator for the given mining job.
    pub fn new(job: &'a MiningJob) -> Self {
        // Copy the template from the job
        let current_extranonce2 = job.extranonce2_template;

        Self {
            job,
            current_extranonce2,
            exhausted: false,
        }
    }

    /// Calculate merkle root for the current extranonce2 value.
    fn calculate_merkle_root(&self) -> sha256d::Hash {
        // Build complete coinbase transaction
        let mut coinbase = Vec::new();
        coinbase.extend_from_slice(&self.job.coinbase1);
        coinbase.extend_from_slice(&self.job.extranonce1);
        self.current_extranonce2.extend_vec(&mut coinbase);
        coinbase.extend_from_slice(&self.job.coinbase2);

        // Calculate coinbase transaction hash (double SHA256)
        let coinbase_hash = sha256d::Hash::hash(&coinbase);

        // Build merkle root from coinbase hash and branches
        let mut root = coinbase_hash;
        for branch in &self.job.merkle_branches {
            // Concatenate current hash with branch hash
            let mut combined = Vec::with_capacity(64);
            combined.extend_from_slice(root.as_byte_array());
            combined.extend_from_slice(branch.as_byte_array());

            // Double SHA256 to get next level
            root = sha256d::Hash::hash(&combined);
        }

        root
    }
}

impl<'a> Iterator for JobWorkIterator<'a> {
    type Item = JobWork;

    fn next(&mut self) -> Option<Self::Item> {
        if self.exhausted {
            return None;
        }

        // Calculate merkle root for current extranonce2
        let merkle_root = self.calculate_merkle_root();

        // Build block header
        let header = BlockHeader {
            version: self.job.version,
            prev_blockhash: self.job.prev_blockhash,
            merkle_root: TxMerkleNode::from_byte_array(merkle_root.to_byte_array()),
            time: self.job.time,
            bits: self.job.bits,
            nonce: 0, // Will be rolled by ASIC
        };

        // Save current extranonce2 for the work unit
        let extranonce2 = self.current_extranonce2;
        let job_id = self.job.job_id.clone();

        // Increment for next iteration
        if !self.current_extranonce2.increment() {
            self.exhausted = true;
        }

        Some(JobWork {
            header,
            extranonce2,
            job_id,
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        if self.exhausted {
            (0, Some(0))
        } else {
            // Calculate remaining iterations based on extranonce2 space
            let total_space = self.job.extranonce2_space();
            // This is approximate since we can't easily track exact position
            // in the general case, but gives a reasonable estimate
            let remaining = if total_space > usize::MAX as u64 {
                (usize::MAX, None)
            } else {
                (0, Some(total_space as usize))
            };
            remaining
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Extranonce2;
    use bitcoin::block::Version;
    use bitcoin::hash_types::{BlockHash, TxMerkleNode};
    use bitcoin::hashes::{sha256d, Hash};
    use bitcoin::pow::CompactTarget;
    use std::str::FromStr;

    fn create_test_job() -> MiningJob {
        // Create a simple test job with minimal valid data
        MiningJob {
            job_id: "test_job_1".to_string(),
            prev_blockhash: BlockHash::all_zeros(),
            version: Version::TWO,
            bits: CompactTarget::from_consensus(0x1d00ffff),
            time: 1234567890,
            coinbase1: vec![0x01, 0x02, 0x03],
            coinbase2: vec![0x04, 0x05, 0x06],
            merkle_branches: vec![],
            extranonce1: vec![0xaa, 0xbb],
            extranonce2_template: Extranonce2::new(2).expect("valid size"),
            clean_jobs: false,
            version_mask: None,
        }
    }

    #[test]
    fn test_job_work_iterator_basic() {
        let job = create_test_job();
        let mut iter = JobWorkIterator::new(&job);

        // First iteration should have extranonce2 = [0x00, 0x00]
        let work = iter.next().unwrap();
        assert_eq!(Vec::<u8>::from(work.extranonce2), vec![0x00, 0x00]);
        assert_eq!(work.job_id, "test_job_1");

        // Second iteration should have extranonce2 = [0x01, 0x00]
        let work = iter.next().unwrap();
        assert_eq!(Vec::<u8>::from(work.extranonce2), vec![0x01, 0x00]);

        // Third iteration should have extranonce2 = [0x02, 0x00]
        let work = iter.next().unwrap();
        assert_eq!(Vec::<u8>::from(work.extranonce2), vec![0x02, 0x00]);
    }

    #[test]
    fn test_extranonce2_rollover() {
        let mut job = create_test_job();
        job.extranonce2_template = Extranonce2::new(1).expect("valid size"); // Single byte for easier testing
        let mut iter = JobWorkIterator::new(&job);

        // Skip to near the end
        for _ in 0..255 {
            assert!(iter.next().is_some());
        }

        // 256th iteration (last valid extranonce2 = 0xFF)
        let work = iter.next().unwrap();
        assert_eq!(Vec::<u8>::from(work.extranonce2), vec![0xFF]);

        // Should be exhausted after 256 iterations (0x00 through 0xFF)
        assert!(iter.next().is_none());
    }

    #[test]
    fn test_merkle_root_calculation() {
        let job = create_test_job();
        let mut iter = JobWorkIterator::new(&job);

        // Get two different work units
        let work1 = iter.next().unwrap();
        let work2 = iter.next().unwrap();

        // Different extranonce2 values should produce different merkle roots
        assert_ne!(work1.header.merkle_root, work2.header.merkle_root);

        // But other header fields should remain the same
        assert_eq!(work1.header.version, work2.header.version);
        assert_eq!(work1.header.prev_blockhash, work2.header.prev_blockhash);
        assert_eq!(work1.header.time, work2.header.time);
        assert_eq!(work1.header.bits, work2.header.bits);
        assert_eq!(work1.header.nonce, work2.header.nonce);
    }

    #[test]
    fn test_merkle_root_with_branches() {
        let mut job = create_test_job();
        // Add some merkle branches (simulating transactions in the block)
        job.merkle_branches = vec![
            TxMerkleNode::from_raw_hash(
                sha256d::Hash::from_str(
                    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                )
                .unwrap(),
            ),
            TxMerkleNode::from_raw_hash(
                sha256d::Hash::from_str(
                    "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210",
                )
                .unwrap(),
            ),
        ];

        let mut iter = JobWorkIterator::new(&job);
        let work = iter.next().unwrap();

        // Verify we get a valid block header with merkle root
        assert_ne!(work.header.merkle_root, TxMerkleNode::all_zeros());
    }

    #[test]
    fn test_size_hint() {
        let mut job = create_test_job();
        job.extranonce2_template = Extranonce2::new(1).expect("valid size"); // 256 possible values

        let iter = JobWorkIterator::new(&job);
        let (lower, upper) = iter.size_hint();

        assert_eq!(lower, 0);
        assert_eq!(upper, Some(256));
    }
}
