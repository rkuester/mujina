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

use std::collections::HashMap;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::hash_thread::{HashThread, HashThreadEvent};
use crate::job_generator::JobGenerator;
use crate::tracing::prelude::*;

// TODO: Future enhancements for frequency ramping:
// - Make ramp parameters configurable (step size, delay, target)
// - Monitor chip temperature/errors during ramp
// - Coordinate with board-level voltage regulators
// - Implement adaptive ramping based on chip response
// - Add rollback on errors during ramp

/// Run the scheduler task, receiving hash threads from the backplane.
pub async fn task(
    running: CancellationToken,
    mut thread_rx: mpsc::Receiver<Vec<Box<dyn HashThread>>>,
) {
    // Wait for the first set of hash threads from the backplane
    let threads = match thread_rx.recv().await {
        Some(threads) => {
            debug!("Received {} hash thread(s) from backplane", threads.len());
            threads
        }
        None => return,
    };

    // Store threads and get their event receivers
    // For now, we only support one thread (Bitaxe Gamma has 1 chip)
    let mut thread = threads
        .into_iter()
        .next()
        .expect("Should have at least one thread");

    debug!("Received hash thread from backplane");

    // Get the event receiver from the thread
    let mut event_rx = match thread.take_event_receiver() {
        Some(rx) => rx,
        None => {
            error!("Thread was not initialized properly - no event receiver available");
            return;
        }
    };

    // Create job generator for testing (using difficulty 1 for easy verification)
    let difficulty = 1;
    let _job_generator = JobGenerator::new(difficulty);
    debug!("Created job generator with difficulty {}", difficulty);

    // Track active jobs for nonce verification
    let _active_jobs: HashMap<u64, crate::asic::MiningJob> = HashMap::new();

    // Track mining statistics
    let mut stats = MiningStats {
        difficulty: difficulty as f64,
        ..Default::default()
    };

    // TODO: Assign initial work to thread via thread.update_work()
    // For now, thread starts idle - work assignment will be implemented later
    debug!("Thread ready (idle, awaiting work assignment implementation)");

    // Main scheduler loop

    while !running.is_cancelled() {
        tokio::select! {
            // Handle hash thread events
            Some(event) = event_rx.recv() => {
                match event {
                    HashThreadEvent::ShareFound(share) => {
                        info!("Share found! Job {} nonce {:#x}", share.job_id, share.nonce);
                        stats.nonces_found += 1;
                        stats.valid_nonces += 1;
                        // TODO: Verify share and submit to pool
                    }

                    HashThreadEvent::WorkExhausted { en2_searched } => {
                        info!("Thread exhausted work (searched {} EN2 values)", en2_searched);
                        stats.jobs_completed += 1;

                        // TODO: Assign new work via thread.update_work()
                        // For now, we don't have work assignment implemented
                        warn!("Work exhausted but new work assignment not yet implemented");
                    }

                    HashThreadEvent::WorkDepletionWarning { estimated_remaining_ms } => {
                        debug!("Work depletion warning: ~{}ms remaining", estimated_remaining_ms);
                        // TODO: Prepare next work assignment
                    }

                    HashThreadEvent::StatusUpdate(status) => {
                        trace!("Thread status: hashrate={:.2} GH/s, active={}",
                               status.hashrate / 1_000_000_000.0, status.is_active);
                    }

                    HashThreadEvent::GoingOffline => {
                        warn!("Hash thread going offline");
                        // Thread is shutting down (board removed, fault, etc.)
                        // TODO: Handle thread removal, reassign work to other threads
                        running.cancel();  // For now, just shut down
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
