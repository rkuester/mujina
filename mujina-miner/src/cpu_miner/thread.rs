//! CPU mining HashThread implementation.
//!
//! Provides the scheduler-facing interface for a CPU mining thread.
//! Uses the actor pattern with a dedicated `std::thread` for hashing.

use std::sync::{
    Arc, RwLock,
    atomic::{AtomicBool, Ordering},
    mpsc,
};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use tokio::sync::mpsc as tokio_mpsc;
use tokio::task::JoinHandle;

use super::attributes::{CpuThreadAttributes, DEFAULT_DUTY};
use super::hasher::{self, MinerCommand};
use crate::{
    asic::hash_thread::{
        HashTask, HashThread, HashThreadCapabilities, HashThreadEvent, HashThreadStatus,
    },
    tracing::prelude::*,
    types::{HashRate, Ratio},
};

/// CPU mining thread implementing the HashThread trait.
///
/// Wraps a `std::thread` running the CPU mining loop and provides
/// the async interface expected by the scheduler.
pub struct CpuHashThread {
    /// Human-readable name for logging.
    name: String,

    /// Channel for sending commands to the mining thread.
    command_tx: mpsc::Sender<MinerCommand>,

    /// The thread's place in the tree, holding the duty a client
    /// asks of it and the hashrate it reports.
    attrs: CpuThreadAttributes,

    /// Event sender for reporting to the scheduler (e.g. expected hashrate).
    event_tx: tokio_mpsc::Sender<HashThreadEvent>,

    /// Event receiver (taken by scheduler).
    event_rx: Option<tokio_mpsc::Receiver<HashThreadEvent>>,

    /// Shared status for queries.
    status: Arc<RwLock<HashThreadStatus>>,

    /// Cached capabilities.
    capabilities: HashThreadCapabilities,

    /// Shutdown flag shared with mining thread.
    shutdown: Arc<AtomicBool>,

    /// Handle to the mining thread (for join on drop).
    _thread_handle: Option<std::thread::JoinHandle<()>>,

    /// Re-declares the expected hashrate when the duty changes.
    duty_watcher: Option<JoinHandle<()>>,
}

impl CpuHashThread {
    /// Create a new CPU mining thread.
    ///
    /// Spawns a dedicated `std::thread` for CPU-bound hashing.
    ///
    /// # Arguments
    ///
    /// * `name` - Human-readable name for logging
    /// * `attrs` - The thread's attributes in the tree
    pub fn new(name: String, attrs: CpuThreadAttributes) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (evt_tx, evt_rx) = tokio_mpsc::channel(100);

        let status = Arc::new(RwLock::new(HashThreadStatus::default()));
        let shutdown = Arc::new(AtomicBool::new(false));

        // Clone for the spawned thread
        let status_clone = Arc::clone(&status);
        let shutdown_clone = Arc::clone(&shutdown);
        let thread_name = name.clone();
        let duty = attrs.target_duty.subscribe();
        let published = attrs.thread.clone();

        // Spawn the mining thread
        let handle = std::thread::Builder::new()
            .name(format!("cpu-miner-{}", name))
            .spawn(move || {
                hasher::run_mining_loop(
                    thread_name,
                    cmd_rx,
                    status_clone,
                    duty,
                    published,
                    shutdown_clone,
                );
            })
            .expect("Failed to spawn CPU mining thread");

        Self {
            name,
            command_tx: cmd_tx,
            attrs,
            event_tx: evt_tx,
            event_rx: Some(evt_rx),
            status,
            capabilities: HashThreadCapabilities::default(),
            shutdown,
            _thread_handle: Some(handle),
            duty_watcher: None,
        }
    }

    /// Signal the mining thread to shut down.
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
        let _ = self.command_tx.send(MinerCommand::Shutdown);
    }

    /// The hashrate this thread expects to sustain at a duty.
    fn expected(peak: HashRate, duty: Option<Ratio>) -> HashRate {
        let duty = duty.unwrap_or(DEFAULT_DUTY);
        HashRate::from_megahashes(peak.as_megahashes() * f64::from(duty.factor()))
    }
}

impl Drop for CpuHashThread {
    fn drop(&mut self) {
        self.shutdown();
        if let Some(watcher) = self.duty_watcher.take() {
            watcher.abort();
        }
    }
}

#[async_trait]
impl HashThread for CpuHashThread {
    fn name(&self) -> &str {
        &self.name
    }

    fn capabilities(&self) -> &HashThreadCapabilities {
        &self.capabilities
    }

    async fn configure(&mut self) -> Result<()> {
        // Measure this CPU's peak rate, then scale by the duty cycle to get
        // the effective rate the thread expects to sustain. Probing beats a
        // fixed constant because peak throughput varies widely across CPUs.
        let peak = tokio::task::spawn_blocking(hasher::probe_peak_hashrate).await?;
        self.event_tx
            .send(HashThreadEvent::ExpectedHashRate(Self::expected(
                peak,
                self.attrs.target_duty.get(),
            )))
            .await
            .map_err(|_| anyhow!("event channel closed"))?;

        // A duty a client changes moves the rate the scheduler
        // should expect, so declare the new one as it happens.
        let mut duty = self.attrs.target_duty.subscribe();
        let events = self.event_tx.clone();
        let name = self.name.clone();
        duty.borrow_and_update();
        self.duty_watcher = Some(tokio::spawn(async move {
            while duty.changed().await.is_ok() {
                let asked = *duty.borrow_and_update();
                debug!(
                    thread = %name,
                    duty = %asked.unwrap_or(DEFAULT_DUTY),
                    "CPU thread duty changed"
                );
                if events
                    .send(HashThreadEvent::ExpectedHashRate(Self::expected(
                        peak, asked,
                    )))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        }));

        Ok(())
    }

    async fn update_task(&mut self, new_task: HashTask) -> Result<Option<HashTask>> {
        let (response_tx, response_rx) = tokio::sync::oneshot::channel();

        self.command_tx
            .send(MinerCommand::UpdateTask {
                task: new_task,
                response_tx,
            })
            .map_err(|_| anyhow!("command channel closed"))?;

        response_rx
            .await
            .map_err(|_| anyhow!("no response from thread"))?
    }

    async fn replace_task(&mut self, new_task: HashTask) -> Result<Option<HashTask>> {
        let (response_tx, response_rx) = tokio::sync::oneshot::channel();

        self.command_tx
            .send(MinerCommand::ReplaceTask {
                task: new_task,
                response_tx,
            })
            .map_err(|_| anyhow!("command channel closed"))?;

        response_rx
            .await
            .map_err(|_| anyhow!("no response from thread"))?
    }

    async fn go_idle(&mut self) -> Result<Option<HashTask>> {
        let (response_tx, response_rx) = tokio::sync::oneshot::channel();

        self.command_tx
            .send(MinerCommand::GoIdle { response_tx })
            .map_err(|_| anyhow!("command channel closed"))?;

        response_rx
            .await
            .map_err(|_| anyhow!("no response from thread"))?
    }

    fn take_event_receiver(&mut self) -> Option<tokio_mpsc::Receiver<HashThreadEvent>> {
        self.event_rx.take()
    }

    fn status(&self) -> HashThreadStatus {
        self.status.read().unwrap().clone()
    }
}
