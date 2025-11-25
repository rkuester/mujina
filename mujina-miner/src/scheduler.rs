//! The scheduler module manages the distribution of mining jobs to hash boards
//! and ASIC chips.
//!
//! # Share Filtering (Three-Layer Architecture)
//!
//! Share filtering happens at three independent levels:
//!
//! **Layer 1 - Chip TicketMask (hardware pre-filter):**
//! - Configured by thread during initialization
//! - Chip only reports nonces meeting this threshold
//! - Set for frequent health signals (~1/sec at current hashrate)
//!
//! **Layer 2 - HashTask.share_target (thread-to-scheduler filter):**
//! - Configured by scheduler when assigning work
//! - Thread validates and emits ShareFound only for shares meeting this
//! - Controls message volume to scheduler
//! - Allows per-thread difficulty adjustment
//!
//! **Layer 3 - JobTemplate.share_target (scheduler-to-source filter):**
//! - Set by pool via Stratum mining.set_difficulty
//! - Scheduler validates before forwarding to source
//! - Only pool-worthy shares submitted
//!
//! The scheduler receives shares meeting HashTask.share_target, uses them for
//! statistics and monitoring, then filters again before pool submission. This
//! provides accurate per-thread metrics while controlling network traffic.
//!
//! This is a work-in-progress. It's currently the main and initial place where
//! functionality is added, after which the functionality is refactored out to
//! where it belongs.

use slotmap::SlotMap;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::{StreamExt, StreamMap};
use tokio_util::sync::CancellationToken;

use crate::hash_thread::{task::HashTask, HashThread, HashThreadEvent};
use crate::job_source::{JobTemplate, MerkleRootKind, SourceCommand, SourceEvent};
use crate::tracing::prelude::*;
use crate::types::HashRate;

/// Unique identifier for a job source, assigned by the scheduler.
pub type SourceId = slotmap::DefaultKey;

/// Unique identifier for a hash thread, assigned by the scheduler.
pub type ThreadId = slotmap::DefaultKey;

/// Association between a job template and its originating source.
///
/// When the scheduler receives a job from a source, it wraps it in ActiveJob
/// to track the source association. HashTasks reference this via Arc, allowing
/// shares to be routed back to the correct source without threads needing to
/// know about sources.
#[derive(Debug, Clone)]
pub struct ActiveJob {
    /// Source that provided this job
    pub source_id: SourceId,

    /// Job template with block header fields
    pub template: JobTemplate,
}

/// Registration message for adding a job source to the scheduler.
///
/// The daemon creates sources and sends this message to register them.
/// The scheduler inserts the source into its SlotMap and begins listening
/// for events.
pub struct SourceRegistration {
    /// Source name for logging
    pub name: String,

    /// Event receiver for this source (UpdateJob, ReplaceJob, ClearJobs)
    pub event_rx: mpsc::Receiver<SourceEvent>,

    /// Command sender for this source (SubmitShare, etc.)
    pub command_tx: mpsc::Sender<SourceCommand>,
}

/// Internal scheduler tracking for a registered source.
struct SourceEntry {
    /// Source name for logging
    name: String,

    /// Command channel for sending to this source
    command_tx: mpsc::Sender<SourceCommand>,
}

/// Calculates aggregate hashrate from all registered threads.
fn total_hashrate_estimate<T>(threads: &SlotMap<ThreadId, T>) -> HashRate
where
    T: std::ops::Deref<Target = dyn HashThread>,
{
    let total: u64 = threads
        .values()
        .map(|t| t.capabilities().hashrate_estimate.0)
        .sum();
    HashRate(total)
}

/// Broadcasts hashrate update to all registered sources.
///
/// Currently sends the same aggregate hashrate to all sources. In the future,
/// this will send per-source allocations based on scheduling policy (e.g., when
/// splitting hashrate across multiple pools).
async fn broadcast_hashrate(sources: &SlotMap<SourceId, SourceEntry>, hashrate: HashRate) {
    for source in sources.values() {
        let _ = source
            .command_tx
            .send(SourceCommand::UpdateHashRate(hashrate))
            .await;
    }
}

// TODO: Future enhancements for frequency ramping:
// - Make ramp parameters configurable (step size, delay, target)
// - Monitor chip temperature/errors during ramp
// - Coordinate with board-level voltage regulators
// - Implement adaptive ramping based on chip response
// - Add rollback on errors during ramp

/// Run the scheduler task, receiving hash threads and job sources.
pub async fn task(
    running: CancellationToken,
    mut thread_rx: mpsc::Receiver<Vec<Box<dyn HashThread>>>,
    mut source_reg_rx: mpsc::Receiver<SourceRegistration>,
) {
    // Source storage and event multiplexing
    let mut sources: SlotMap<SourceId, SourceEntry> = SlotMap::new();
    let mut source_events: StreamMap<SourceId, ReceiverStream<SourceEvent>> = StreamMap::new();

    // Thread storage and event multiplexing
    let mut threads: SlotMap<ThreadId, Box<dyn HashThread>> = SlotMap::new();
    let mut thread_events: StreamMap<ThreadId, ReceiverStream<HashThreadEvent>> = StreamMap::new();

    // Track which job each thread is working on
    let mut thread_assignments: HashMap<ThreadId, Arc<ActiveJob>> = HashMap::new();

    // Wait for the first set of hash threads from the backplane
    let initial_threads = match thread_rx.recv().await {
        Some(threads) => threads,
        None => return,
    };

    if initial_threads.is_empty() {
        error!("No hash threads received from backplane");
        return;
    }

    debug!(
        "Received {} hash thread(s) from backplane",
        initial_threads.len()
    );

    // Insert threads into SlotMap and StreamMap
    for mut thread in initial_threads {
        let event_rx = thread
            .take_event_receiver()
            .expect("Thread missing event receiver");

        let thread_id = threads.insert(thread);
        thread_events.insert(thread_id, ReceiverStream::new(event_rx));
        debug!(thread_id = ?thread_id, "Thread registered");
    }

    // Track mining statistics
    let mut stats = MiningStats::default();

    // Create interval for periodic status logging
    let mut status_interval = tokio::time::interval(tokio::time::Duration::from_secs(30));
    status_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut first_tick = true;

    debug!("Scheduler ready (awaiting job sources)");

    // Track thread count to detect disconnections
    let mut last_thread_count = threads.len();

    // Main scheduler loop

    while !running.is_cancelled() {
        tokio::select! {
            // Source registration
            Some(registration) = source_reg_rx.recv() => {
                let source_id = sources.insert(SourceEntry {
                    name: registration.name.clone(),
                    command_tx: registration.command_tx,
                });
                source_events.insert(source_id, ReceiverStream::new(registration.event_rx));
                debug!(source_id = ?source_id, name = %registration.name, "Source registered");

                // Send current hashrate estimate to the new source
                let hashrate = total_hashrate_estimate(&threads);
                if let Some(source) = sources.get(source_id) {
                    let _ = source.command_tx.send(SourceCommand::UpdateHashRate(hashrate)).await;
                }
            }

            // Source events
            Some((source_id, event)) = source_events.next() => {
                let source = sources.get(source_id)
                    .expect("StreamMap returned invalid source_id");

                // TODO: Factor out common job assignment logic (EN2 extraction, ActiveJob
                // creation, range splitting, work assignment) into helper function
                match event {
                    SourceEvent::UpdateJob(job_template) => {
                        debug!(
                            source = %source.name,
                            job_id = %job_template.id,
                            "UpdateJob received"
                        );

                        // Extract EN2 range (only supported for computed merkle roots)
                        let full_en2_range = match &job_template.merkle_root {
                            MerkleRootKind::Computed(template) => template.extranonce2_range.clone(),
                            MerkleRootKind::Fixed(_) => {
                                error!(job_id = %job_template.id, "Header-only jobs not supported");
                                continue;
                            }
                        };

                        // Create active job with source association
                        let active_job = Arc::new(ActiveJob {
                            source_id,
                            template: job_template,
                        });

                        // Split EN2 range among all threads
                        let en2_slices = full_en2_range.split(threads.len())
                            .expect("Failed to split EN2 range among threads");

                        // Assign work to all threads
                        for ((thread_id, thread), en2_range) in threads.iter_mut().zip(en2_slices) {
                            let starting_en2 = en2_range.iter().next();

                            let task = HashTask {
                                job: active_job.clone(),
                                en2_range: Some(en2_range),
                                en2: starting_en2,
                                share_target: active_job.template.share_target,
                                ntime: active_job.template.time,
                            };

                            if let Err(e) = thread.update_work(task).await {
                                error!(thread_id = ?thread_id, error = %e, "Failed to assign work");
                            } else {
                                thread_assignments.insert(thread_id, active_job.clone());
                            }
                        }
                    }

                    SourceEvent::ReplaceJob(job_template) => {
                        debug!(
                            source = %source.name,
                            job_id = %job_template.id,
                            "ReplaceJob received"
                        );

                        // Extract EN2 range (only supported for computed merkle roots)
                        let full_en2_range = match &job_template.merkle_root {
                            MerkleRootKind::Computed(template) => template.extranonce2_range.clone(),
                            MerkleRootKind::Fixed(_) => {
                                error!(job_id = %job_template.id, "Header-only jobs not supported");
                                continue;
                            }
                        };

                        // Create active job with source association
                        let active_job = Arc::new(ActiveJob {
                            source_id,
                            template: job_template,
                        });

                        // Split EN2 range among all threads
                        let en2_slices = full_en2_range.split(threads.len())
                            .expect("Failed to split EN2 range among threads");

                        // Replace work on all threads (old shares invalid)
                        for ((thread_id, thread), en2_range) in threads.iter_mut().zip(en2_slices) {
                            let starting_en2 = en2_range.iter().next();

                            let task = HashTask {
                                job: active_job.clone(),
                                en2_range: Some(en2_range),
                                en2: starting_en2,
                                share_target: active_job.template.share_target,
                                ntime: active_job.template.time,
                            };

                            if let Err(e) = thread.replace_work(task).await {
                                error!(thread_id = ?thread_id, error = %e, "Failed to replace work");
                            } else {
                                thread_assignments.insert(thread_id, active_job.clone());
                            }
                        }
                    }

                    SourceEvent::ClearJobs => {
                        debug!(source = %source.name, "ClearJobs received");

                        let affected_threads: Vec<ThreadId> = thread_assignments
                            .iter()
                            .filter(|(_, job)| job.source_id == source_id)
                            .map(|(tid, _)| *tid)
                            .collect();

                        for tid in affected_threads {
                            if let Some(thread) = threads.get_mut(tid) {
                                if let Err(e) = thread.go_idle().await {
                                    error!(thread_id = ?tid, error = %e, "Failed to idle thread");
                                }
                            }
                            thread_assignments.remove(&tid);
                        }
                    }
                }
            }

            // Thread events
            Some((thread_id, event)) = thread_events.next() => {
                match event {
                    HashThreadEvent::ShareFound(share) => {
                        debug!(
                            thread_id = ?thread_id,
                            job_id = %share.task.job.template.id,
                            nonce = format!("{:#x}", share.nonce),
                            hash = %share.hash,
                            "Share found"
                        );

                        // Track hashes for hashrate measurement
                        // Use threshold difficulty, not achieved difficulty (see MiningStats doc)
                        let hashes = (share.threshold_difficulty * (u32::MAX as f64 + 1.0)) as u128;
                        stats.total_hashes += hashes;

                        // Check if share meets source threshold
                        let source_id = share.task.job.source_id;
                        let template = &share.task.job.template;

                        if template.share_target.is_met_by(share.hash) {
                            stats.shares_submitted += 1;

                            // Submit share to originating source
                            if let Some(source) = sources.get(source_id) {
                                use crate::job_source::Share as SourceShare;
                                let source_share = SourceShare {
                                    job_id: template.id.clone(),
                                    nonce: share.nonce,
                                    time: share.ntime,
                                    version: share.version,
                                    extranonce2: share.extranonce2,
                                };

                                if let Err(e) = source.command_tx.send(SourceCommand::SubmitShare(source_share)).await {
                                    error!(
                                        source_id = ?source_id,
                                        error = %e,
                                        "Failed to submit share to source"
                                    );
                                } else {
                                    debug!(source = %source.name, "Share submitted to source");
                                }
                            } else {
                                error!(source_id = ?source_id, "Share for unknown source");
                            }
                        } else {
                            trace!(
                                thread_id = ?thread_id,
                                nonce = format!("{:#x}", share.nonce),
                                "Share below source threshold (not submitted)"
                            );
                        }
                    }

                    HashThreadEvent::WorkExhausted { en2_searched } => {
                        info!(thread_id = ?thread_id, en2_searched, "Work exhausted");
                        // TODO: Assign new work to this thread
                    }

                    HashThreadEvent::WorkDepletionWarning { estimated_remaining_ms } => {
                        debug!(thread_id = ?thread_id, remaining_ms = estimated_remaining_ms, "Work depletion warning");
                        // TODO: Prepare next work assignment
                    }

                    HashThreadEvent::StatusUpdate(status) => {
                        trace!(
                            thread_id = ?thread_id,
                            hashrate = %status.hashrate.to_human_readable(),
                            active = status.is_active,
                            "Thread status"
                        );
                    }
                }
            }

            // New thread batches from backplane
            Some(new_threads) = thread_rx.recv() => {
                if new_threads.is_empty() {
                    continue;
                }

                debug!("Received {} new hash thread(s)", new_threads.len());

                for mut thread in new_threads {
                    let event_rx = thread
                        .take_event_receiver()
                        .expect("Thread missing event receiver");

                    let thread_id = threads.insert(thread);
                    thread_events.insert(thread_id, ReceiverStream::new(event_rx));
                    debug!(thread_id = ?thread_id, "Thread registered");
                }

                // Broadcast updated hashrate to all sources
                let hashrate = total_hashrate_estimate(&threads);
                broadcast_hashrate(&sources, hashrate).await;

                last_thread_count = thread_events.len();
            }

            // Periodic status check
            _ = status_interval.tick() => {
                if first_tick {
                    first_tick = false;
                } else {
                    stats.log_summary();
                }
            }

            // Shutdown
            _ = running.cancelled() => {
                debug!("Scheduler shutdown requested");
                break;
            }
        }

        // Detect thread disconnections (StreamMap silently removes ended streams)
        // Check thread_events since that's where disconnections are detected.
        let current_count = thread_events.len();
        if current_count != last_thread_count {
            debug!(
                previous = last_thread_count,
                current = current_count,
                "Thread count changed"
            );

            // Remove threads that no longer have active event streams
            let active_ids: std::collections::HashSet<_> = thread_events.keys().collect();
            threads.retain(|id, _| active_ids.contains(&id));
            thread_assignments.retain(|id, _| active_ids.contains(id));

            last_thread_count = current_count;

            // Broadcast updated hashrate to all sources
            let hashrate = total_hashrate_estimate(&threads);
            broadcast_hashrate(&sources, hashrate).await;
        }
    }

    // Log final statistics
    stats.log_summary();

    debug!("Scheduler shutdown complete");
}

/// Mining statistics tracker
///
/// # Hashrate Calculation Methodology
///
/// We calculate hashrate using **threshold difficulty**, not achieved difficulty.
///
/// ## Statistical Model
///
/// - Chip hashes at constant rate (what we want to measure)
/// - Shares meeting threshold D_t arrive as Poisson process
/// - Achieved difficulty follows exponential distribution (memoryless)
/// - Each share represents expected work: D_t * 2^32 hashes
///
/// ## Comparison to Pool Statistics
///
/// Mining pools (like hydrapool) use achieved difficulty because they don't
/// control miner thresholds. We control our threshold, so we can use it
/// directly for more stable estimates.
///
/// Using achieved difficulty introduces high variance from outliers. One lucky
/// difficulty-10M share would dominate the average, incorrectly inflating
/// hashrate estimates. Threshold-based calculation is variance-minimizing.
struct MiningStats {
    start_time: std::time::Instant,
    /// Total hashes performed (accumulated across all shares).
    ///
    /// Uses u128 for overflow safety. At 1 TH/s, u64 would overflow in 5 hours
    /// but u128 won't overflow for 10 quadrillion years.
    total_hashes: u128,
    shares_submitted: u64,
}

impl Default for MiningStats {
    fn default() -> Self {
        let now = std::time::Instant::now();
        Self {
            start_time: now,
            total_hashes: 0,
            shares_submitted: 0,
        }
    }
}

impl MiningStats {
    fn log_summary(&mut self) {
        let elapsed = self.start_time.elapsed().as_secs_f64();

        // Calculate hashrate from accumulated hashes
        let hashrate = if elapsed > 0.0 && self.total_hashes > 0 {
            let hashrate_hs = self.total_hashes as f64 / elapsed;
            Some(HashRate(hashrate_hs as u64))
        } else {
            None
        };

        // Mining statistics
        if let Some(rate) = hashrate {
            info!(
                uptime_s = elapsed as u64,
                hashrate = %rate.to_human_readable(),
                shares = self.shares_submitted,
                "Mining status."
            );
        } else {
            info!(
                uptime_s = elapsed as u64,
                shares = self.shares_submitted,
                "Mining status."
            );
        }
    }
}
