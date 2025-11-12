//! The scheduler module manages the distribution of mining jobs to hash boards
//! and ASIC chips.
//!
//! # Share Filtering
//!
//! The scheduler receives ALL shares from HashThreads and performs final
//! filtering before forwarding to JobSources:
//!
//! - HashThreads forward all chip shares (hash already computed)
//! - Scheduler filters by job target (only pool-worthy shares submitted)
//! - Scheduler uses all shares for per-thread hashrate measurement
//! - Scheduler tracks chip health across all threads
//!
//! This centralized filtering provides accurate monitoring while keeping
//! thread implementations simple.
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

    debug!("Scheduler ready (awaiting job sources)");

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
                        stats.nonces_found += 1;
                        stats.valid_nonces += 1;
                        // TODO: Verify share and submit to pool
                    }

                    HashThreadEvent::WorkExhausted { en2_searched } => {
                        info!(thread_id = ?thread_id, en2_searched, "Work exhausted");
                        stats.jobs_completed += 1;
                        // TODO: Assign new work to this thread
                    }

                    HashThreadEvent::WorkDepletionWarning { estimated_remaining_ms } => {
                        debug!(thread_id = ?thread_id, remaining_ms = estimated_remaining_ms, "Work depletion warning");
                        // TODO: Prepare next work assignment
                    }

                    HashThreadEvent::StatusUpdate(status) => {
                        trace!(
                            thread_id = ?thread_id,
                            hashrate_ghs = format!("{:.2}", status.hashrate / 1_000_000_000.0),
                            active = status.is_active,
                            "Thread status"
                        );
                    }
                }
            }

            // Periodic status check
            _ = tokio::time::sleep(tokio::time::Duration::from_secs(30)) => {
                trace!("Scheduler heartbeat - mining active");
                stats.log_summary();
            }

            // Shutdown
            _ = running.cancelled() => {
                debug!("Scheduler shutdown requested");
                break;
            }
        }
    }

    // Log final statistics
    stats.log_summary();

    debug!("Scheduler shutdown complete");
}

/// Mining statistics tracker
struct MiningStats {
    nonces_found: u64,
    valid_nonces: u64,
    invalid_nonces: u64,
    jobs_completed: u64,
    start_time: std::time::Instant,
    difficulty: f64,
}

impl Default for MiningStats {
    fn default() -> Self {
        let now = std::time::Instant::now();
        Self {
            nonces_found: 0,
            valid_nonces: 0,
            invalid_nonces: 0,
            jobs_completed: 0,
            start_time: now,

            difficulty: 1.0,
        }
    }
}

impl MiningStats {
    fn log_summary(&mut self) {
        let elapsed = self.start_time.elapsed().as_secs_f64();

        // Theoretical hashrate based on chip specifications
        const TARGET_FREQUENCY_MHZ: f32 = 500.0;
        const BM1370_HASH_ENGINES: f64 = 1280.0;
        let theoretical_hashrate_mhs = TARGET_FREQUENCY_MHZ as f64 * BM1370_HASH_ENGINES;

        let valid_pct = if self.nonces_found > 0 {
            self.valid_nonces as f64 / self.nonces_found as f64 * 100.0
        } else {
            0.0
        };

        // Basic statistics
        info!(
            uptime_s = elapsed as u64,
            difficulty = self.difficulty,
            theoretical_mhs = format!("{:.2}", theoretical_hashrate_mhs),
            nonces_found = self.nonces_found,
            valid = self.valid_nonces,
            valid_pct = format!("{:.2}", valid_pct),
            invalid = self.invalid_nonces,
            jobs_completed = self.jobs_completed,
            "Mining statistics"
        );

        // Measured hashrate (if we have data)
        if elapsed > 0.0 && self.valid_nonces > 0 {
            let hashes_per_nonce = self.difficulty * (u32::MAX as f64 + 1.0);
            let estimated_total_hashes = self.valid_nonces as f64 * hashes_per_nonce;
            let measured_hashrate_mhs = (estimated_total_hashes / elapsed) / 1_000_000.0;
            let efficiency = (measured_hashrate_mhs / theoretical_hashrate_mhs) * 100.0;

            debug!(
                measured_mhs = format!("{:.2}", measured_hashrate_mhs),
                efficiency_pct = format!("{:.1}", efficiency),
                sample_size = self.valid_nonces,
                "Measured hashrate"
            );
        }

        // Poisson analysis (if applicable)
        if elapsed > 0.0 && theoretical_hashrate_mhs > 0.0 {
            let expected_rate = (theoretical_hashrate_mhs * 1_000_000.0)
                / (self.difficulty * (u32::MAX as f64 + 1.0));
            let expected_nonces = expected_rate * elapsed;

            if expected_nonces > 1.0 {
                let std_dev = expected_nonces.sqrt();
                let lower = (expected_nonces - 2.0 * std_dev).max(0.0);
                let upper = expected_nonces + 2.0 * std_dev;

                debug!(
                    expected = format!("{:.1}", expected_nonces),
                    found = self.valid_nonces,
                    ci_lower = format!("{:.1}", lower),
                    ci_upper = format!("{:.1}", upper),
                    "Poisson analysis (95% CI)"
                );
            }
        }
    }
}
