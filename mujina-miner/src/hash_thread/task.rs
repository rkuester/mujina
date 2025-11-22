//! HashTask and Share types for work assignment and result reporting.

use std::sync::Arc;

use bitcoin::block::Version;
use bitcoin::pow::Target;
use bitcoin::BlockHash;

use crate::job_source::{Extranonce2, Extranonce2Range};
use crate::scheduler::ActiveJob;

/// Work assignment from scheduler to hash thread.
///
/// Represents actual mining work from a job source (pool or dummy). Contains
/// the job (template + source association), the extranonce2 range allocated
/// to this thread, and state for resumable work iteration.
///
/// The scheduler maps jobs back to sources via the ActiveJob. Threads don't
/// need to know about sources. If a thread has no HashTask (None), it's idle
/// (low power, no hashing).
#[derive(Debug, Clone)]
pub struct HashTask {
    /// Job to work on (template + source association)
    pub job: Arc<ActiveJob>,

    /// Extranonce2 range allocated to this thread.
    ///
    /// None for header-only mining (Stratum v2). Current HashThread
    /// implementations require EN2 iteration, so None will cause errors
    /// until header-only support is added.
    pub en2_range: Option<Extranonce2Range>,

    /// Extranonce2 value.
    ///
    /// When scheduler assigns work: starting EN2.
    /// When stored as snapshot: the EN2 value that was used.
    /// None for header-only mining (Stratum v2).
    pub en2: Option<Extranonce2>,

    /// Share target for thread-to-scheduler submission threshold.
    ///
    /// Thread emits ShareFound only for shares meeting this target. Allows
    /// scheduler to control message volume independently from pool submission
    /// difficulty. Typically set easier than source threshold for monitoring.
    pub share_target: Target,

    /// Current ntime value
    ///
    /// May be rolled forward during mining. To start, uses the job's time field.
    pub ntime: u32,
}

/// Valid share found by a HashThread.
///
/// Hash has been computed and verified against the job target. Scheduler uses
/// the task reference to route shares back to the originating source.
#[derive(Debug, Clone)]
pub struct Share {
    /// Task this share solves (contains job template and source mapping)
    pub task: Arc<HashTask>,

    /// Winning nonce
    pub nonce: u32,

    /// Computed block hash
    pub hash: BlockHash,

    /// Threshold difficulty this share was validated against.
    ///
    /// This represents the expected hashing work, not the achieved difficulty.
    /// Used for hashrate calculation where each share represents the same
    /// expected work regardless of its actual hash difficulty (which is luck).
    pub threshold_difficulty: f64,

    /// Version bits
    pub version: Version,

    /// Block timestamp
    pub ntime: u32,

    /// Extranonce2 value used (None in, e.g., header-only mining in Stratum v2)
    pub extranonce2: Option<Extranonce2>,
}
