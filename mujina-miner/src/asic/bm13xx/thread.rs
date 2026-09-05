//! BM13xx HashThread implementation.
//!
//! This module provides the HashThread implementation for BM13xx family ASIC
//! chips (BM1362, BM1366, BM1370, etc.). A BM13xxThread represents a chain of
//! BM13xx chips connected via a shared serial bus.
//!
//! The thread is implemented as an actor task that monitors the serial bus for
//! chip responses, filters shares, and manages work assignment.

use std::cmp::max;
use std::ops::ControlFlow;
use std::sync::{Arc, RwLock};

use anyhow::{Context as _, Result, anyhow, bail};
use async_trait::async_trait;
use bitcoin::block::Header as BlockHeader;
use futures::{SinkExt, stream::Stream};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::{self, Duration, MissedTickBehavior};

use super::chain::Chain;
use super::chip_config::ChipConfig;
use super::command::{
    ChainInactive, ChipCommandSink, Destination, JobCommand, JobFullFormat, RegisterCommand,
    SetChipAddress, SinkError, WriteRegister,
};
use super::peripherals::BoardPeripherals;
use super::reader::{Reader, ReaderChannels};
use super::register::{
    AdcCtrl1, CoreCommand, CoreRegister, IoDriverStrength, Log2Difficulty, MidstateConfig,
    PllDivider, Register, RegisterAddress, TicketMask,
};
use super::register_client::RegisterClient;
use super::response::{NonceFields, NonceResponse, RegisterResponse, Response};
use super::topology::TopologySpec;
use crate::{
    asic::hash_thread::{
        HashTask, HashThread, HashThreadCapabilities, HashThreadEvent, HashThreadStatus, Share,
        ThreadAttributes,
    },
    systree::{Cursor, Report, Setting, within},
    tracing::prelude::*,
    types::{Difficulty, Frequency, HashRate, HashrateEstimator, ShareRate, Voltage},
};

/// BM13xx HashThread implementation.
///
/// Represents a chain of BM13xx chips as a schedulable worker. The thread
/// manages serial communication with chips, filters shares, and reports events.
/// Chain initialization happens lazily when first work is assigned.
pub struct BM13xxThread {
    /// Human-readable name for logging
    name: String,

    /// Channel for sending commands to the actor
    command_tx: mpsc::Sender<ThreadCommand>,

    /// Event receiver (taken by scheduler)
    event_rx: Option<mpsc::Receiver<HashThreadEvent>>,

    /// Cached capabilities
    capabilities: HashThreadCapabilities,

    /// Shared status (updated by actor task)
    status: Arc<RwLock<HashThreadStatus>>,
}

impl BM13xxThread {
    /// Create a new BM13xx thread with Stream/Sink for chip communication
    ///
    /// Thread starts with the chips held in reset. The chain will be
    /// initialized when first work is assigned.
    ///
    /// # Arguments
    /// * `name` - Human-readable name for logging (e.g., "Bitaxe Gamma (e2f56f9b)")
    /// * `attrs` - The thread's attributes in the tree, under which
    ///   it publishes what each chip does
    /// * `config` - Chip model configuration (identity, PLL parameters)
    /// * `topology` - The board's declared chip wiring
    /// * `chip_responses` - Stream of decoded responses from chips
    /// * `chip_commands` - Sink for sending encoded commands to chips
    /// * `peripherals` - Hardware interfaces from board (reset line, regulator, etc.)
    /// * `shutdown_rx` - Shutdown signal from the board; a send or a
    ///   dropped sender requests shutdown
    #[expect(
        clippy::too_many_arguments,
        reason = "one argument per thing the board supplies"
    )]
    pub fn new<R, W>(
        name: String,
        attrs: ThreadAttributes,
        config: ChipConfig,
        topology: TopologySpec,
        chip_responses: R,
        chip_commands: W,
        peripherals: BoardPeripherals,
        shutdown_rx: watch::Receiver<()>,
    ) -> Self
    where
        R: Stream<Item = Result<Response, std::io::Error>> + Unpin + Send + 'static,
        W: ChipCommandSink + Unpin + Send + 'static,
        SinkError<W>: std::error::Error + Send + Sync + 'static,
    {
        let (command_tx, command_rx) = mpsc::channel(10);
        let (event_tx, event_rx) = mpsc::channel(100);

        let status = Arc::new(RwLock::new(HashThreadStatus::default()));

        let (reader, channels) = Reader::spawn(chip_responses);
        let actor = Actor::new(
            attrs,
            config,
            topology,
            event_tx,
            Arc::clone(&status),
            chip_commands,
            peripherals,
            reader,
        );
        tokio::spawn(actor.run(command_rx, shutdown_rx, channels));

        Self {
            name,
            command_tx,
            event_rx: Some(event_rx),
            capabilities: HashThreadCapabilities::default(),
            status,
        }
    }
}

#[async_trait]
impl HashThread for BM13xxThread {
    fn name(&self) -> &str {
        &self.name
    }

    fn capabilities(&self) -> &HashThreadCapabilities {
        &self.capabilities
    }

    async fn configure(&mut self) -> Result<()> {
        self.command_tx
            .send(ThreadCommand::Configure)
            .await
            .map_err(|_| anyhow!("command channel closed"))
    }

    async fn update_task(&mut self, new_task: HashTask) -> Result<Option<HashTask>> {
        let (response_tx, response_rx) = oneshot::channel();

        self.command_tx
            .send(ThreadCommand::UpdateTask {
                new_task,
                response_tx,
            })
            .await
            .map_err(|_| anyhow!("command channel closed"))?;

        response_rx
            .await
            .map_err(|_| anyhow!("no response from thread"))?
    }

    async fn replace_task(&mut self, new_task: HashTask) -> Result<Option<HashTask>> {
        let (response_tx, response_rx) = oneshot::channel();

        self.command_tx
            .send(ThreadCommand::ReplaceTask {
                new_task,
                response_tx,
            })
            .await
            .map_err(|_| anyhow!("command channel closed"))?;

        response_rx
            .await
            .map_err(|_| anyhow!("no response from thread"))?
    }

    async fn go_idle(&mut self) -> Result<Option<HashTask>> {
        let (response_tx, response_rx) = oneshot::channel();

        self.command_tx
            .send(ThreadCommand::GoIdle { response_tx })
            .await
            .map_err(|_| anyhow!("command channel closed"))?;

        response_rx
            .await
            .map_err(|_| anyhow!("no response from thread"))?
    }

    async fn set_power_fraction(&mut self, fraction: f32) -> Result<()> {
        self.command_tx
            .send(ThreadCommand::SetPowerFraction { fraction })
            .await
            .map_err(|_| anyhow!("command channel closed"))
    }

    fn take_event_receiver(&mut self) -> Option<mpsc::Receiver<HashThreadEvent>> {
        self.event_rx.take()
    }

    fn status(&self) -> HashThreadStatus {
        self.status.read().unwrap().clone()
    }
}

/// Command messages sent from scheduler to thread
#[derive(Debug)]
enum ThreadCommand {
    /// Declare expected hashrate and ready the thread for work
    Configure,

    /// Update task (old shares still valid)
    UpdateTask {
        new_task: HashTask,
        response_tx: oneshot::Sender<Result<Option<HashTask>>>,
    },

    /// Replace task (old shares invalid)
    ReplaceTask {
        new_task: HashTask,
        response_tx: oneshot::Sender<Result<Option<HashTask>>>,
    },

    /// Run at the scheduler's share of full power
    SetPowerFraction { fraction: f32 },

    /// Go idle (stop hashing, low power)
    GoIdle {
        response_tx: oneshot::Sender<Result<Option<HashTask>>>,
    },
}

/// Internal actor for BM13xxThread.
///
/// The channels the select loop awaits are `run` parameters rather
/// than fields, so the loop can borrow them independently of the
/// actor state.
struct Actor<W> {
    /// Chip model configuration (identity, PLL parameters).
    config: ChipConfig,

    /// Live model of the chip chain, built from the board's declared
    /// topology.
    chain: Chain,

    /// Event channel to the scheduler.
    event_tx: mpsc::Sender<HashThreadEvent>,

    /// Shared status, read by the handle.
    status: Arc<RwLock<HashThreadStatus>>,

    /// Sink for sending encoded commands to chips.
    chip_commands: W,

    /// Hardware interfaces from the board (reset line, regulator, etc.).
    peripherals: BoardPeripherals,

    /// Owner of the response demux task. Held only so the task is
    /// aborted, releasing the serial stream, when the actor exits.
    _reader: Reader,

    /// ASIC ticket mask difficulty.
    asic_difficulty: Log2Difficulty,

    /// The thread's attributes in the tree.
    thread_attrs: ThreadAttributes,

    /// The thread's place in the tree.
    sys: Cursor,

    /// The share of full power a client asks of this thread. Unset
    /// means the scheduler decides.
    target_power_fraction: Setting<Option<f32>>,

    /// The share of full power the scheduler allocated.
    scheduled_fraction: f32,

    /// What each chip has done, by chain index.
    chips: Vec<ChipTelemetry>,

    /// Whether lazy chain initialization has run.
    chain_initialized: bool,

    /// The frequency the chips run at, or `None` while they sit in
    /// reset.
    current_freq: Option<Frequency>,

    /// A ramp in progress, stepped from the actor loop so chip
    /// responses keep draining while the chain moves.
    ramp: Option<Ramp>,

    /// The task currently being hashed.
    current_task: Option<HashTask>,

    /// Tasks sent to the chip, by chip job id.
    chip_jobs: ChipJobTracker,
}

impl<W> Actor<W>
where
    W: ChipCommandSink + Unpin,
    SinkError<W>: std::error::Error + Send + Sync + 'static,
{
    #[expect(
        clippy::too_many_arguments,
        reason = "one argument per thing the board supplies"
    )]
    fn new(
        thread_attrs: ThreadAttributes,
        config: ChipConfig,
        topology: TopologySpec,
        event_tx: mpsc::Sender<HashThreadEvent>,
        status: Arc<RwLock<HashThreadStatus>>,
        chip_commands: W,
        peripherals: BoardPeripherals,
        reader: Reader,
    ) -> Self {
        // ASIC ticket mask difficulty: ~1 nonce/sec at nameplate rate
        let asic_difficulty = Log2Difficulty::from_difficulty(
            ShareRate::per_second(1.0).to_difficulty(config.nameplate),
        );

        let chain = Chain::from_topology(&topology);
        let sys = thread_attrs.cursor().clone();
        let sys_chips = sys.child("chips");
        let chips = (0..chain.chip_count())
            .map(|i| ChipTelemetry::attach(&sys_chips.child(&i.to_string())))
            .collect();

        Self {
            config,
            chain,
            event_tx,
            status,
            chip_commands,
            peripherals,
            _reader: reader,
            asic_difficulty,
            target_power_fraction: sys.setting_checked(
                "target_power_fraction",
                None,
                within(0.0..=1.0),
            ),
            scheduled_fraction: 1.0,
            thread_attrs,
            sys,
            chips,
            chain_initialized: false,
            current_freq: None,
            ramp: None,
            current_task: None,
            chip_jobs: ChipJobTracker::new(),
        }
    }

    /// Runs the actor loop until shutdown or channel closure.
    ///
    /// Handles commands from the scheduler (update/replace work, go
    /// idle), the shutdown signal from the board (USB unplug,
    /// fault, etc.), and the demuxed chip responses from the reader.
    /// Reset is asserted on startup to establish known state; the
    /// chain is initialized lazily when the scheduler assigns first
    /// work.
    async fn run(
        mut self,
        mut command_rx: mpsc::Receiver<ThreadCommand>,
        mut shutdown_rx: watch::Receiver<()>,
        channels: ReaderChannels,
    ) {
        let ReaderChannels {
            mut nonces,
            mut register_responses,
        } = channels;

        // Assert reset on startup to establish known state
        if let Err(e) = self.peripherals.reset_line.assert().await {
            warn!(error = %e, "Failed to assert chip reset on startup");
        }

        let mut ntime_ticker = time::interval(Duration::from_secs(1));
        ntime_ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

        let mut telemetry_ticker = time::interval(Duration::from_secs(1));
        telemetry_ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

        let mut ramp_ticker = time::interval(RAMP_STEP_INTERVAL);
        ramp_ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

        // Subscribe before the loop, so a write between the two
        // wakes the loop rather than passing unseen.
        let mut client_fraction = self.target_power_fraction.subscribe();
        client_fraction.borrow_and_update();

        loop {
            tokio::select! {
                // Shutdown signal (highest priority); an error means
                // the board dropped the sender, which requests
                // shutdown too
                _ = shutdown_rx.changed() => {
                    self.set_active(false);

                    // Exit actor loop (channel closure signals exit to the scheduler)
                    break;
                }

                // Commands from scheduler
                cmd = command_rx.recv() => {
                    let Some(cmd) = cmd else {
                        debug!("Thread handle dropped");
                        break;
                    };
                    match cmd {
                        ThreadCommand::Configure => self.configure().await,

                        ThreadCommand::UpdateTask { new_task, response_tx } => {
                            let flow = self
                                .assign_task(new_task, response_tx, false, &mut shutdown_rx, &mut register_responses)
                                .await;
                            if flow.is_break() {
                                break;
                            }
                        }

                        ThreadCommand::ReplaceTask { new_task, response_tx } => {
                            let flow = self
                                .assign_task(new_task, response_tx, true, &mut shutdown_rx, &mut register_responses)
                                .await;
                            if flow.is_break() {
                                break;
                            }
                        }

                        ThreadCommand::GoIdle { response_tx } => {
                            debug!("Going idle");

                            let old_task = self.current_task.take();
                            self.set_active(false);
                            response_tx.send(Ok(old_task)).ok();
                        }

                        ThreadCommand::SetPowerFraction { fraction } => {
                            self.scheduled_fraction = fraction;
                            if self
                                .apply_power_fraction(&mut shutdown_rx, &mut register_responses)
                                .await
                                .is_break()
                            {
                                break;
                            }
                        }
                    }
                }

                // A client's own power fraction for this thread
                Ok(()) = client_fraction.changed() => {
                    client_fraction.borrow_and_update();
                    if self
                        .apply_power_fraction(&mut shutdown_rx, &mut register_responses)
                        .await
                        .is_break()
                    {
                        break;
                    }
                }

                // Nonce reports from the chips
                nonce = nonces.recv() => {
                    let Some(nonce) = nonce else {
                        warn!("Chip response stream ended");
                        break;
                    };
                    self.handle_nonce(nonce).await;
                }

                // Replies to register conversations; nothing asks
                // yet, so log and discard
                response = register_responses.recv() => {
                    let Some(response) = response else {
                        warn!("Chip response stream ended");
                        break;
                    };
                    trace!(
                        chip_address = %format!("0x{:02x}", response.chip_address),
                        register = ?response.register,
                        "Register read response"
                    );
                    if let Register::CoreRegisterValue(_) = response.register
                        && let Some(id) = self.chain.index_of_address(response.chip_address)
                    {
                        self.chips[id.index()].core_register_reports += 1;
                    }
                }

                _ = telemetry_ticker.tick() => {
                    self.publish_chips();
                }

                // One step of a ramp in progress
                _ = ramp_ticker.tick(), if self.ramp.is_some() => {
                    if let Err(e) = self.ramp_step().await {
                        error!(error = %e, "Failed to move the chain");
                        self.ramp = None;
                    }
                    if self.ramp.is_none() {
                        self.declare_expected_hashrate().await;
                    }
                }

                // ntime rolling timer (roll forward every second)
                _ = ntime_ticker.tick(), if self.current_task.is_some() && self.chain_initialized => {
                    self.roll_ntime().await;
                }
            }
        }

        self.disable_chain().await;
        debug!("BM13xx thread actor exiting");
    }

    /// Asserts the chips' reset, then disables the core rail.
    /// Idempotent, and safe before bring-up. On an unplugged
    /// board both writes fail; the warnings are all that can be
    /// done.
    async fn disable_chain(&mut self) {
        if let Err(e) = self.peripherals.reset_line.assert().await {
            warn!(error = %e, "Failed to assert chip reset on exit");
        }
        if let Err(e) = self.peripherals.voltage_regulator.disable().await {
            warn!(error = %e, "Failed to disable core voltage on exit");
        }
        self.current_freq = None;
        self.ramp = None;
    }

    /// Declares the thread's expected hashrate to the scheduler.
    async fn configure(&mut self) {
        self.declare_expected_hashrate().await;
    }

    /// The share of full power the thread runs at: the client's
    /// own setting when it stands, else the scheduler's allocation.
    fn effective_power_fraction(&self) -> f32 {
        self.target_power_fraction
            .get()
            .unwrap_or(self.scheduled_fraction)
    }

    /// The frequency the chain should run at, or `None` for off.
    fn target_frequency(&self) -> Option<Frequency> {
        self.config
            .frequency_for_power_fraction(self.effective_power_fraction())
    }

    /// The rate the chain sustains at its target frequency: the
    /// nameplate scaled by the clock, and zero when off.
    fn expected_hashrate(&self) -> HashRate {
        match self.target_frequency() {
            Some(freq) => {
                let scale = freq.hz() as f64 / self.config.default_freq.hz() as f64;
                let full = self.config.nameplate * self.chain.chip_count();
                HashRate::from_megahashes(full.as_megahashes() * scale)
            }
            None => HashRate::default(),
        }
    }

    /// Tells the scheduler and the tree the rate to expect at the
    /// current operating point.
    async fn declare_expected_hashrate(&mut self) {
        let expected = self.expected_hashrate();
        self.thread_attrs
            .write()
            .set(&self.thread_attrs.hashrate, expected);
        if self
            .event_tx
            .send(HashThreadEvent::ExpectedHashRate(expected))
            .await
            .is_err()
        {
            debug!("Event channel closed while declaring the hashrate");
        }
    }

    /// Moves the chain to the operating point the effective power
    /// fraction asks for: off when it is zero, brought up when it
    /// was off and work is waiting, ramped otherwise. Returns
    /// `Break` when the board's shutdown signal cut a bring-up
    /// short.
    async fn apply_power_fraction(
        &mut self,
        shutdown_rx: &mut watch::Receiver<()>,
        register_responses: &mut mpsc::Receiver<RegisterResponse>,
    ) -> ControlFlow<()> {
        let fraction = self.effective_power_fraction();
        match self.target_frequency() {
            None => {
                if self.chain_initialized {
                    info!(fraction, "Power fraction is zero; holding the chain off");
                    self.disable_chain().await;
                    self.chain_initialized = false;
                    self.chip_jobs.clear();
                    self.set_active(false);
                }
            }
            Some(target) if self.chain_initialized => {
                if self.current_freq != Some(target) {
                    info!(
                        fraction,
                        target_mhz = target.mhz(),
                        "Moving the chain to a new operating point"
                    );
                    if let Err(e) = self.start_ramp(target).await {
                        error!(error = %e, "Failed to move the chain");
                    }
                }
            }
            Some(target) => {
                let Some(task) = self.current_task.clone() else {
                    return ControlFlow::Continue(());
                };
                info!(fraction, target_mhz = target.mhz(), "Bringing the chain up");
                tokio::select! {
                    result = self.initialize_chain(target, register_responses) => {
                        match result {
                            Ok(()) => {
                                self.chain_initialized = true;
                                match self.send_job(&task).await {
                                    Ok(()) => self.set_active(true),
                                    Err(e) => error!(error = %e, "Failed to send the waiting job"),
                                }
                            }
                            Err(e) => error!(error = %format!("{e:#}"), "Chain initialization failed"),
                        }
                    }
                    _ = shutdown_rx.changed() => {
                        debug!("Shutdown requested during bring-up");
                        return ControlFlow::Break(());
                    }
                }
            }
        }
        self.declare_expected_hashrate().await;
        ControlFlow::Continue(())
    }

    /// Takes a new task and sends its first job to the chip,
    /// initializing the chain on the first assignment. `replace`
    /// forgets prior jobs, invalidating their shares. Returns
    /// `Break` when the actor must exit because the board's
    /// shutdown signal cut bring-up short.
    async fn assign_task(
        &mut self,
        new_task: HashTask,
        response_tx: oneshot::Sender<Result<Option<HashTask>>>,
        replace: bool,
        shutdown_rx: &mut watch::Receiver<()>,
        register_responses: &mut mpsc::Receiver<RegisterResponse>,
    ) -> ControlFlow<()> {
        let verb = if replace { "Replacing" } else { "Updating" };
        if let Some(ref old) = self.current_task {
            debug!(
                old_job = %old.template.id,
                new_job = %new_task.template.id,
                "{verb} work"
            );
        } else {
            debug!(new_job = %new_task.template.id, "{verb} work from idle");
        }

        // The select watches for shutdown while bring-up runs, so
        // every await point in bring-up is an abort point.
        // Dropping the half-done future is safe because the actor
        // disables the chain on exit whatever the bring-up
        // progress.
        tokio::select! {
            result = self.ensure_chain_initialized(register_responses) => {
                if let Err(e) = result {
                    error!(error = %format!("{e:#}"), "Chain initialization failed");
                    response_tx.send(Err(e)).ok();
                    return ControlFlow::Continue(());
                }
            }

            _ = shutdown_rx.changed() => {
                debug!("Shutdown requested during bring-up");
                response_tx.send(Err(anyhow!("shut down during bring-up"))).ok();
                return ControlFlow::Break(());
            }
        }

        if replace {
            // Clear old jobs (old shares invalid)
            self.chip_jobs.clear();
        }

        let old_task = self.current_task.replace(new_task.clone());

        // A chain held off keeps the task for when it comes back
        if !self.chain_initialized {
            debug!("Chain is off; holding the work");
            self.set_active(false);
            response_tx.send(Ok(old_task)).ok();
            return ControlFlow::Continue(());
        }

        // Send the task's first job; the ntime roller sends the rest
        if let Err(e) = self.send_job(&new_task).await {
            error!(error = %e, "Failed to send first job to chip");
            response_tx.send(Err(e)).ok();
            return ControlFlow::Continue(());
        }
        if replace {
            debug!("Sent first job to chip (old work invalidated)");
        } else {
            debug!("Sent first job to chip");
        }

        self.set_active(true);
        response_tx.send(Ok(old_task)).ok();
        ControlFlow::Continue(())
    }

    /// Sends a task's job to the chain under a fresh chip job id.
    async fn send_job(&mut self, task: &HashTask) -> Result<()> {
        let chip_job_id = self.chip_jobs.insert(task.clone());
        let job_data = task_to_job_full(task, chip_job_id).context("failed to convert task")?;
        self.chip_commands
            .send(JobCommand::JobFull(job_data))
            .await
            .map_err(|e| anyhow!("failed to send job to chip: {e:?}"))
    }

    /// Initializes the chain on the first call; later calls are
    /// no-ops.
    async fn ensure_chain_initialized(
        &mut self,
        register_responses: &mut mpsc::Receiver<RegisterResponse>,
    ) -> Result<()> {
        if self.chain_initialized {
            return Ok(());
        }
        let Some(target) = self.target_frequency() else {
            debug!("Power fraction is zero; leaving the chain off");
            return Ok(());
        };

        trace!("Initializing chain on first assignment.");
        self.initialize_chain(target, register_responses).await?;
        self.chain_initialized = true;
        Ok(())
    }

    /// Initializes the chip chain for mining.
    ///
    /// Powers the core rail, releases the chips from reset,
    /// enumerates them against the declared topology, assigns
    /// addresses, configures all registers, and ramps the frequency
    /// to `target`.
    async fn initialize_chain(
        &mut self,
        target: Frequency,
        register_responses: &mut mpsc::Receiver<RegisterResponse>,
    ) -> Result<()> {
        // Power the core rail before releasing reset, at the voltage
        // the chips need at their reset frequency
        self.set_core_voltage(*self.config.freq_range.start())
            .await?;
        debug!("Enabling core voltage");
        self.peripherals
            .voltage_regulator
            .enable()
            .await
            .context("failed to enable core voltage")?;
        time::sleep(Duration::from_millis(500)).await;

        // Release the chips from reset
        debug!("Releasing chip reset");
        self.peripherals
            .reset_line
            .release()
            .await
            .context("failed to release chip reset")?;
        self.current_freq = Some(*self.config.freq_range.start());

        time::sleep(Duration::from_millis(200)).await;

        // Send version mask configuration (3 times)
        debug!("Configuring version mask");
        for _ in 1..=3 {
            self.chip_commands
                .send(RegisterCommand::WriteRegister(WriteRegister {
                    destination: Destination::Broadcast,
                    register: Register::MidstateConfig(MidstateConfig::full_rolling()),
                }))
                .await
                .context("failed to send version mask")?;
            time::sleep(Duration::from_millis(5)).await;
        }

        time::sleep(Duration::from_millis(10)).await;

        // Enumerate the chips and check them against the declared
        // topology. The version mask above switched the chips to the
        // 11-byte response format the codec parses.
        debug!("Enumerating chips");
        let replies = RegisterClient::new(&mut self.chip_commands, register_responses)
            .broadcast_read(RegisterAddress::ChipId)
            .await
            .context("chip enumeration failed")?;
        if replies.len() != self.chain.chip_count() {
            bail!(
                "found {} chips, declared topology has {}",
                replies.len(),
                self.chain.chip_count()
            );
        }
        debug!(chips = replies.len(), "Chip enumeration complete");

        // Pre-configuration registers
        debug!("Sending pre-configuration registers");

        self.chip_commands
            .send(RegisterCommand::WriteRegister(WriteRegister {
                destination: Destination::Broadcast,
                register: Register::SoftResetControl(self.config.soft_reset_defaults),
            }))
            .await?;
        self.chip_commands
            .send(RegisterCommand::WriteRegister(WriteRegister {
                destination: Destination::Broadcast,
                register: Register::MiscControl(self.config.misc_control),
            }))
            .await?;

        self.chip_commands
            .send(RegisterCommand::ChainInactive(ChainInactive))
            .await
            .context("failed to send ChainInactive")?;

        // Address the chips in chain order. After ChainInactive, the
        // first unaddressed chip adopts each SetChipAddress and
        // forwards later ones downstream, so one command per chip
        // addresses the whole chain.
        self.chain
            .assign_addresses()
            .context("chip address assignment failed")?;
        {
            let mut published = self.sys.write();
            for (id, chip) in self.chain.chips() {
                published.set(&self.chips[id.index()].attrs.address, chip.address);
            }
        }
        for (_, chip) in self.chain.chips() {
            self.chip_commands
                .send(RegisterCommand::SetChipAddress(SetChipAddress {
                    chip_address: chip.address,
                }))
                .await
                .context("failed to send SetChipAddress")?;
        }

        // Core configuration (broadcast)
        debug!("Sending broadcast core configuration");

        self.chip_commands
            .send(RegisterCommand::WriteRegister(WriteRegister {
                destination: Destination::Broadcast,
                register: Register::CoreMailbox(self.config.clock_select),
            }))
            .await?;
        self.chip_commands
            .send(RegisterCommand::WriteRegister(WriteRegister {
                destination: Destination::Broadcast,
                register: Register::CoreMailbox(CoreCommand::write_all(
                    CoreRegister::ClockDelay,
                    0x0C,
                )),
            }))
            .await?;

        // Ticket mask
        let ticket_mask = TicketMask::new(self.asic_difficulty);

        self.chip_commands
            .send(RegisterCommand::WriteRegister(WriteRegister {
                destination: Destination::Broadcast,
                register: Register::TicketMask(ticket_mask),
            }))
            .await?;
        self.chip_commands
            .send(RegisterCommand::WriteRegister(WriteRegister {
                destination: Destination::Broadcast,
                register: Register::IoDriverStrength(IoDriverStrength::normal()),
            }))
            .await?;

        // Chip-specific configuration
        debug!("Sending chip-specific configuration");

        for (_, chip) in self.chain.chips() {
            let destination = Destination::Chip(chip.address);
            self.chip_commands
                .send(RegisterCommand::WriteRegister(WriteRegister {
                    destination,
                    register: Register::SoftResetControl(self.config.core_reset),
                }))
                .await?;
            self.chip_commands
                .send(RegisterCommand::WriteRegister(WriteRegister {
                    destination,
                    register: Register::MiscControl(self.config.misc_control),
                }))
                .await?;
            self.chip_commands
                .send(RegisterCommand::WriteRegister(WriteRegister {
                    destination,
                    register: Register::CoreMailbox(self.config.clock_select),
                }))
                .await?;
            self.chip_commands
                .send(RegisterCommand::WriteRegister(WriteRegister {
                    destination,
                    register: Register::CoreMailbox(CoreCommand::write_all(
                        CoreRegister::ClockDelay,
                        0x0C,
                    )),
                }))
                .await?;
            self.chip_commands
                .send(RegisterCommand::WriteRegister(WriteRegister {
                    destination,
                    register: Register::CoreMailbox(CoreCommand::write_all(
                        CoreRegister::CoreEnable,
                        0xAA,
                    )),
                }))
                .await?;
        }

        // Additional settings
        self.chip_commands
            .send(RegisterCommand::WriteRegister(WriteRegister {
                destination: Destination::Broadcast,
                register: Register::AdcCtrl1(AdcCtrl1::bring_up()),
            }))
            .await?;
        self.chip_commands
            .send(RegisterCommand::WriteRegister(WriteRegister {
                destination: Destination::Broadcast,
                register: Register::AnalogMux(self.config.analog_mux),
            }))
            .await?;
        self.chip_commands
            .send(RegisterCommand::WriteRegister(WriteRegister {
                destination: Destination::Broadcast,
                register: Register::AdcCtrl1(AdcCtrl1::bring_up()),
            }))
            .await?;
        self.chip_commands
            .send(RegisterCommand::WriteRegister(WriteRegister {
                destination: Destination::Broadcast,
                register: Register::CoreMailbox(CoreCommand::nonce_bin_overflow(true)),
            }))
            .await?;

        // Frequency ramping from the reset frequency to the target;
        // the sweep deadline for the target is written along the way
        self.ramp_to(target).await?;

        // Final configuration
        self.chip_commands
            .send(RegisterCommand::WriteRegister(WriteRegister {
                destination: Destination::Broadcast,
                register: Register::MidstateConfig(MidstateConfig::full_rolling()),
            }))
            .await?;

        time::sleep(Duration::from_millis(150)).await;

        // Verify bring-up by reading configuration back from every
        // chip at its assigned address; a directed read answered at
        // that address also proves the chip took it.
        debug!("Verifying chain configuration");
        let target_pll = self
            .config
            .calculate_pll(target)
            .context("no PLL solution for the target frequency")?;
        // What each chip should answer. The registers answer as
        // written, except PLL_DIVIDER bit 31, the lock report
        // (LOCKED), which a healthy chip answers set after the ramp.
        let expected = [
            Register::MiscControl(self.config.misc_control),
            Register::TicketMask(ticket_mask),
            Register::PllDivider(PllDivider {
                locked: true,
                ..target_pll
            }),
        ];
        let addresses: Vec<u8> = self.chain.chips().map(|(_, chip)| chip.address).collect();
        let mut client = RegisterClient::new(&mut self.chip_commands, register_responses);
        for address in addresses {
            for register in &expected {
                let actual = client
                    .read(address, register.address())
                    .await
                    .context("bring-up verification read failed")?;
                if actual != *register {
                    bail!(
                        "chip 0x{address:02x} readback mismatch: \
                         expected {register:?}, read {actual:?}"
                    );
                }
            }
        }
        debug!("Chain configuration verified");

        Ok(())
    }

    /// Starts a ramp from the frequency the chips run at to
    /// `target`. The actor loop steps it; a ramp already in progress
    /// is replaced from wherever it got to.
    ///
    /// The sweep deadline for the target is written before a rising
    /// ramp and after a falling one. A deadline sized for a slower
    /// clock is too long for a faster one, and a sweep that outruns
    /// its slice wraps and repeats work, so the shorter of the two
    /// deadlines holds throughout.
    async fn start_ramp(&mut self, target: Frequency) -> Result<()> {
        let from = self
            .current_freq
            .context("cannot ramp a chain that is in reset")?;
        let rising = target >= from;
        debug!(
            "Ramping frequency from {} MHz to {} MHz",
            from.mhz(),
            target.mhz()
        );
        if rising {
            self.write_hash_counting_number(target).await?;
        }
        let steps =
            generate_frequency_ramp_steps(&self.config, from, target, self.config.ramp_step);
        self.ramp = Some(Ramp {
            rising,
            steps,
            next: 0,
        });
        Ok(())
    }

    /// Writes every core's sweep deadline for the clock at `freq`.
    async fn write_hash_counting_number(&mut self, freq: Frequency) -> Result<()> {
        let pll = self
            .config
            .calculate_pll(freq)
            .context("no PLL solution for the frequency")?;
        let deadline = self
            .config
            .hash_counting_number(pll, self.chain.chip_count());
        trace!(
            freq_mhz = freq.mhz(),
            ?deadline,
            "Writing the sweep deadline"
        );
        self.chip_commands
            .send(RegisterCommand::WriteRegister(WriteRegister {
                destination: Destination::Broadcast,
                register: Register::HashCountingNumber(deadline),
            }))
            .await
            .context("failed to write the sweep deadline")
    }

    /// Ramps to `target` and waits for the ramp to finish, for
    /// bring-up, where the chain sends nothing the loop must drain.
    async fn ramp_to(&mut self, target: Frequency) -> Result<()> {
        self.start_ramp(target).await?;
        while self.ramp.is_some() {
            self.ramp_step().await?;
            time::sleep(RAMP_STEP_INTERVAL).await;
        }
        Ok(())
    }

    /// Takes one step of the ramp in progress, moving the rail with
    /// the clock: ahead of it on the way up, behind it on the way
    /// down, so the chips never run a clock the rail does not yet
    /// support. Clears the ramp after its last step.
    async fn ramp_step(&mut self) -> Result<()> {
        let Some(ramp) = &self.ramp else {
            return Ok(());
        };
        let rising = ramp.rising;
        let total = ramp.steps.len();
        let index = ramp.next;
        let (freq, pll_config) = ramp.steps[index];

        if rising {
            self.set_core_voltage(freq).await?;
            time::sleep(VOLTAGE_LEAD).await;
        }

        self.chip_commands
            .send(RegisterCommand::WriteRegister(WriteRegister {
                destination: Destination::Broadcast,
                register: Register::PllDivider(pll_config),
            }))
            .await
            .context("PLL ramp failed")?;

        if !rising {
            time::sleep(VOLTAGE_LEAD).await;
            self.set_core_voltage(freq).await?;
        }
        self.current_freq = Some(freq);

        if index % 10 == 0 || index + 1 == total {
            trace!("Frequency ramp step {}/{}", index + 1, total);
        }

        if index + 1 == total {
            debug!("Frequency ramping complete");
            self.ramp = None;
            if !rising {
                self.write_hash_counting_number(freq).await?;
            }
        } else if let Some(ramp) = &mut self.ramp {
            ramp.next = index + 1;
        }
        Ok(())
    }

    /// Sets the core rail to what the chips need at `freq`.
    ///
    /// The rail stacks one voltage per domain in series. A chip
    /// model without a measured curve leaves the rail where the
    /// board set it.
    async fn set_core_voltage(&mut self, freq: Frequency) -> Result<()> {
        let Some(curve) = &self.config.core_voltage else {
            return Ok(());
        };
        let per_chip = curve.voltage_at(freq);
        let rail = Voltage::from_volts(per_chip.volts() * self.chain.domain_count() as f32);
        self.peripherals
            .voltage_regulator
            .set_voltage(rail)
            .await
            .with_context(|| format!("failed to set the core rail to {rail} for {freq}"))?;
        trace!(%freq, %per_chip, %rail, "Core rail set for the frequency");
        Ok(())
    }

    /// Names the chip a nonce came from, by chain index.
    ///
    /// A BM1362 seeds its nonce counter with its address, and the
    /// chain's even addresses keep that address readable in every
    /// nonce. A BM1370 does not, so its nonces are attributed only on
    /// a chain of one.
    fn chip_for_nonce(&self, nonce: u32) -> Option<usize> {
        match NonceFields::from_nonce(self.config.model, nonce) {
            NonceFields::BM1362 { chip_address, .. } => self
                .chain
                .index_of_address(chip_address)
                .map(|id| id.index()),
            NonceFields::BM1370 { .. } => (self.chain.chip_count() == 1).then_some(0),
        }
    }

    /// Publishes every chip's counts and rate under one guard.
    fn publish_chips(&mut self) {
        let rows: Vec<_> = self
            .chips
            .iter_mut()
            .map(|chip| {
                (
                    chip.nonces,
                    chip.hardware_errors,
                    chip.core_register_reports,
                    chip.estimator.hashrate(),
                )
            })
            .collect();
        let mut published = self.sys.write();
        for (chip, (nonces, hardware_errors, core_register_reports, hashrate)) in
            self.chips.iter().zip(rows)
        {
            published.set(&chip.attrs.nonces, nonces);
            published.set(&chip.attrs.hardware_errors, hardware_errors);
            published.set(&chip.attrs.core_register_reports, core_register_reports);
            published.set(&chip.attrs.hashrate, hashrate);
        }
    }

    /// Handles one nonce report from the chips.
    async fn handle_nonce(&mut self, nonce_response: NonceResponse) {
        let NonceResponse {
            nonce,
            job_id,
            version,
            excess_difficulty,
            subcore_id,
        } = nonce_response;

        let chip = self.chip_for_nonce(nonce);
        if let Some(i) = chip {
            self.chips[i].nonces += 1;
        }

        // Look up the task for this job_id
        if let Some(task) = self.chip_jobs.get(job_id) {
            let template = task.template.as_ref();

            // Reconstruct full version from rolling field
            let full_version = version.apply_to_version(template.version.base());

            // Compute merkle root for this task's EN2
            match task
                .en2
                .as_ref()
                .and_then(|en2| template.compute_merkle_root(en2).ok())
            {
                Some(merkle_root) => {
                    // Build block header
                    let header = BlockHeader {
                        version: full_version,
                        prev_blockhash: template.prev_blockhash,
                        merkle_root,
                        time: task.ntime,
                        bits: template.bits,
                        nonce,
                    };

                    // Compute hash
                    let hash = header.block_hash();

                    // A hash under the chip's own ticket mask is a
                    // hardware error; one over it is a sample of the
                    // mask's expected work, whatever the source's
                    // target makes of it.
                    if let Some(i) = chip {
                        let telemetry = &mut self.chips[i];
                        if Difficulty::from_hash(&hash) >= self.asic_difficulty.to_difficulty() {
                            telemetry.estimator.record(self.asic_difficulty.to_work());
                        } else {
                            telemetry.hardware_errors += 1;
                        }
                    }

                    // Validate against task share target
                    if task.share_target.is_met_by(hash) {
                        // Attribute work at the harder of the
                        // ASIC ticket mask and the scheduler
                        // target, since the actual filter is
                        // whichever is stricter.
                        let expected_work =
                            max(self.asic_difficulty.to_work(), task.share_target.to_work());

                        let share = Share {
                            nonce,
                            hash,
                            version: full_version,
                            ntime: task.ntime,
                            extranonce2: task.en2,
                            expected_work,
                        };

                        // Send via task's dedicated channel
                        if task.share_tx.send(share).await.is_err() {
                            // Channel closed = task replaced, share is stale
                            debug!("Share channel closed (task replaced)");
                        } else {
                            debug!(
                                chip_job_id = job_id,
                                nonce = format!("{:#x}", nonce),
                                hash = %hash,
                                hash_diff = %Difficulty::from_hash(&hash),
                                target_diff = %Difficulty::from_target(task.share_target),
                                "Share found and sent"
                            );
                        }
                    } else {
                        trace!(
                            chip_job_id = job_id,
                            nonce = format!("{:#x}", nonce),
                            hash = %hash,
                            hash_diff = %Difficulty::from_hash(&hash),
                            target_diff = %Difficulty::from_target(task.share_target),
                            "Nonce does not meet target (filtered)"
                        );
                    }
                }
                None => {
                    error!(
                        chip_job_id = job_id,
                        "Failed to compute merkle root for nonce"
                    );
                }
            }
        } else {
            trace!(
                chip_job_id = job_id,
                nonce = format!("{:#x}", nonce),
                "Nonce for unknown job_id (possibly stale)"
            );
        }

        let _ = (excess_difficulty, subcore_id); // Unused for now
    }

    /// Rolls the current task's ntime forward and sends the job to
    /// the chip.
    async fn roll_ntime(&mut self) {
        let task = self.current_task.as_mut().unwrap();

        // Increment ntime
        task.ntime += 1;

        // Convert to chip format and send
        match task_to_job_full(task, self.chip_jobs.insert(task.clone())) {
            Ok(job_data) => {
                if let Err(e) = self.chip_commands.send(JobCommand::JobFull(job_data)).await {
                    error!(error = ?e, "Failed to send JobFull to chip");
                } else {
                    trace!(ntime = task.ntime, "Sent ntime-rolled job to chip");
                }
            }
            Err(e) => {
                error!(error = %e, "Failed to convert task to JobFull");
            }
        }
    }

    /// Updates the shared active flag.
    fn set_active(&self, is_active: bool) {
        self.status.write().unwrap().is_active = is_active;
    }
}

/// Tracks tasks sent to chip hardware, indexed by chip_job_id.
///
/// BM13xx chips use 4-bit job IDs. This tracker maintains snapshots of
/// HashTasks sent to the chip so we can match nonce responses back to the
/// correct task context (EN2, ntime, etc.).
struct ChipJobTracker {
    tasks: [Option<HashTask>; 16],
    next_id: u8,
}

impl ChipJobTracker {
    fn new() -> Self {
        Self {
            tasks: Default::default(),
            next_id: 0,
        }
    }

    fn insert(&mut self, task: HashTask) -> u8 {
        let chip_job_id = self.next_id;
        self.tasks[chip_job_id as usize] = Some(task);
        self.next_id = (self.next_id + 1) % (self.tasks.len() as u8);
        chip_job_id
    }

    fn get(&self, chip_job_id: u8) -> Option<&HashTask> {
        self.tasks
            .get(chip_job_id as usize)
            .and_then(|t| t.as_ref())
    }

    fn clear(&mut self) {
        self.tasks = Default::default();
    }
}

/// Convert HashTask to JobFullFormat for chip hardware.
///
/// Extracts or computes the merkle root, then builds a JobFullFormat with all
/// block header fields. For computed merkle roots, requires EN2. For fixed merkle
/// roots (Stratum v2 header-only), uses the template's fixed value directly.
fn task_to_job_full(task: &HashTask, chip_job_id: u8) -> Result<JobFullFormat> {
    use crate::job_source::MerkleRootKind;

    let template = task.template.as_ref();

    // Get merkle root (computed or fixed)
    let merkle_root = match &template.merkle_root {
        MerkleRootKind::Computed(_) => {
            // Extract EN2 (required for computed merkle roots)
            let en2 = task
                .en2
                .as_ref()
                .ok_or_else(|| anyhow!("EN2 required for computed merkle root"))?;

            // Compute merkle root for this EN2
            template
                .compute_merkle_root(en2)
                .context("merkle root computation failed")?
        }
        MerkleRootKind::Fixed(merkle_root) => *merkle_root,
    };

    Ok(JobFullFormat {
        job_id: chip_job_id,
        num_midstates: 1,
        starting_nonce: 0,
        nbits: template.bits,
        ntime: task.ntime,
        merkle_root,
        prev_block_hash: template.prev_blockhash,
        version: template.version.base(),
    })
}

/// Settle time between raising the rail and writing the clock
/// that needs it.
const VOLTAGE_LEAD: Duration = Duration::from_millis(50);

/// Time between one ramp step and the next.
const RAMP_STEP_INTERVAL: Duration = Duration::from_millis(100);

/// A frequency ramp in progress.
struct Ramp {
    /// Whether the clock is rising, which decides whether the rail
    /// moves before or after each PLL write.
    rising: bool,
    /// Every step of the ramp, the first being the frequency the
    /// chips were at.
    steps: Vec<(Frequency, PllDivider)>,
    /// The step to take next.
    next: usize,
}

/// The window a chip's hashrate is estimated over.
const CHIP_HASHRATE_WINDOW: Duration = Duration::from_secs(5 * 60);

/// What one chip has done, kept by the actor and published each
/// second.
struct ChipTelemetry {
    attrs: ChipAttributes,
    estimator: HashrateEstimator,
    nonces: u64,
    hardware_errors: u64,
    core_register_reports: u64,
}

impl ChipTelemetry {
    fn attach(sys: &Cursor) -> Self {
        Self {
            attrs: ChipAttributes::attach(sys),
            estimator: HashrateEstimator::new(CHIP_HASHRATE_WINDOW),
            nonces: 0,
            hardware_errors: 0,
            core_register_reports: 0,
        }
    }
}

/// What one chip holds in the tree, under its thread.
struct ChipAttributes {
    /// The address the chip took at enumeration.
    address: Report<u8>,

    /// Nonces the chip has reported since the thread started.
    nonces: Report<u64>,

    /// Reported nonces whose hash fell short of the chip's own
    /// ticket mask.
    hardware_errors: Report<u64>,

    /// Core register value responses the chip sent with no read
    /// from the host.
    core_register_reports: Report<u64>,

    /// The rate estimated from the chip's nonces over the last few
    /// minutes.
    hashrate: Report<HashRate>,
}

impl ChipAttributes {
    fn attach(sys: &Cursor) -> Self {
        Self {
            address: sys.report("address", 0),
            nonces: sys.report("nonces", 0),
            hardware_errors: sys.report("hardware_errors", 0),
            core_register_reports: sys.report("core_register_reports", 0),
            hashrate: sys.report("hashes", HashRate::default()),
        }
    }
}

/// Generates a ramp's steps from `from` to `target` in either
/// direction, each frequency with the PLL dividers that produce
/// it, for smooth PLL transitions.
///
/// The first step is `from` itself and the last is `target`; a
/// final short step ends the ramp exactly on the target.
fn generate_frequency_ramp_steps(
    config: &ChipConfig,
    from: Frequency,
    target: Frequency,
    step: Frequency,
) -> Vec<(Frequency, PllDivider)> {
    let mut configs = Vec::new();
    let mut current = from;
    let rising = target >= from;

    loop {
        if let Some(pll) = config.calculate_pll(current) {
            configs.push((current, pll));
        }
        if current == target {
            break;
        }
        current = if rising {
            let next = Frequency::from_hz(current.hz() + step.hz());
            if next > target { target } else { next }
        } else {
            let next = Frequency::from_hz(current.hz().saturating_sub(step.hz()));
            if next < target { target } else { next }
        };
    }

    configs
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::task::{Context, Poll};

    use futures::{Sink, stream};

    use super::*;
    use crate::asic::bm13xx::chip_config;
    use crate::asic::bm13xx::peripherals::ResetLine;
    use crate::peripheral::regulator::VoltageRegulator;
    use crate::systree::{Tree, Unconfigured};
    use crate::types::Voltage;

    #[test]
    fn ramp_covers_range_in_steps() {
        let config = chip_config::bm1370();
        let steps = generate_frequency_ramp_steps(
            &config,
            Frequency::from_mhz(56.25),
            Frequency::from_mhz(525.0),
            Frequency::from_mhz(6.25),
        );

        // 75 steps of 6.25 MHz above the starting frequency
        assert_eq!(steps.len(), 76);

        // Each step is the solver's answer for the next stepped
        // frequency
        for (i, (freq, pll)) in steps.iter().enumerate() {
            let expected = Frequency::from_hz(56_250_000 + i as u64 * 6_250_000);
            assert_eq!(*freq, expected, "step {i}");
            assert_eq!(*pll, config.calculate_pll(expected).unwrap(), "step {i}");
        }
    }

    #[test]
    fn ramp_descends_to_a_lower_target() {
        let config = chip_config::bm1370();
        let steps = generate_frequency_ramp_steps(
            &config,
            Frequency::from_mhz(525.0),
            Frequency::from_mhz(500.0),
            Frequency::from_mhz(6.25),
        );

        let freqs: Vec<u64> = steps.iter().map(|(f, _)| f.hz()).collect();
        assert_eq!(
            freqs,
            [
                525_000_000,
                518_750_000,
                512_500_000,
                506_250_000,
                500_000_000
            ]
        );
    }

    #[test]
    fn ramp_stays_put_when_already_on_target() {
        let config = chip_config::bm1370();
        let steps = generate_frequency_ramp_steps(
            &config,
            Frequency::from_mhz(400.0),
            Frequency::from_mhz(400.0),
            Frequency::from_mhz(6.25),
        );

        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].0, Frequency::from_mhz(400.0));
    }

    #[test]
    fn ramp_ends_on_target_when_step_overshoots() {
        let config = chip_config::bm1370();
        let steps = generate_frequency_ramp_steps(
            &config,
            Frequency::from_mhz(56.25),
            Frequency::from_mhz(60.0),
            Frequency::from_mhz(6.25),
        );

        let expected: Vec<_> = [Frequency::from_mhz(56.25), Frequency::from_mhz(60.0)]
            .into_iter()
            .map(|freq| (freq, config.calculate_pll(freq).unwrap()))
            .collect();
        assert_eq!(steps, expected);
    }

    #[test]
    fn test_task_to_job_full_converts_high_level_types() {
        use crate::asic::bm13xx::test_data::esp_miner_job;
        use crate::job_source::{
            Extranonce2, GeneralPurposeBits, JobTemplate, MerkleRootKind, VersionTemplate,
        };

        // Create a JobTemplate with test data values
        // Use MerkleRootKind::Fixed with the exact merkle_root from capture
        let template = Arc::new(JobTemplate {
            id: "test".into(),
            prev_blockhash: *esp_miner_job::wire_tx::PREV_BLOCKHASH,
            version: VersionTemplate::new(
                *esp_miner_job::wire_tx::VERSION,
                GeneralPurposeBits::full(),
            )
            .expect("Valid version template"),
            bits: *esp_miner_job::wire_tx::NBITS,
            share_target: crate::types::Difficulty::from(100_u64).to_target(),
            time: *esp_miner_job::wire_tx::NTIME,
            merkle_root: MerkleRootKind::Fixed(*esp_miner_job::wire_tx::MERKLE_ROOT),
        });

        // Dummy EN2 (doesn't matter since we're using Fixed merkle root)
        let dummy_en2 = Extranonce2::new(0, 1).unwrap();

        // Create dummy channel (not used in this test, just for struct construction)
        let (share_tx, _share_rx) = mpsc::channel(1);

        let task = HashTask {
            template,
            en2_range: None,
            en2: Some(dummy_en2),
            share_target: crate::types::Difficulty::from(100_u64).to_target(),
            ntime: *esp_miner_job::wire_tx::NTIME,
            share_tx,
        };

        // Convert to JobFullFormat
        let result = task_to_job_full(&task, *esp_miner_job::wire_tx::JOB_ID).unwrap();

        // Verify all fields match expected Bitcoin types
        assert_eq!(result.job_id, *esp_miner_job::wire_tx::JOB_ID);
        assert_eq!(result.num_midstates, 1);
        assert_eq!(result.starting_nonce, 0);
        assert_eq!(result.nbits, *esp_miner_job::wire_tx::NBITS);
        assert_eq!(result.ntime, *esp_miner_job::wire_tx::NTIME);
        assert_eq!(result.version, *esp_miner_job::wire_tx::VERSION);
        assert_eq!(
            result.prev_block_hash,
            *esp_miner_job::wire_tx::PREV_BLOCKHASH
        );
        assert_eq!(result.merkle_root, *esp_miner_job::wire_tx::MERKLE_ROOT);
    }

    /// Sink that accepts and discards every command.
    struct NullSink;

    impl Sink<RegisterCommand> for NullSink {
        type Error = io::Error;

        fn poll_ready(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
            Poll::Ready(Ok(()))
        }

        fn start_send(self: Pin<&mut Self>, _: RegisterCommand) -> Result<(), io::Error> {
            Ok(())
        }

        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
            Poll::Ready(Ok(()))
        }

        fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
            Poll::Ready(Ok(()))
        }
    }

    impl Sink<JobCommand> for NullSink {
        type Error = io::Error;

        fn poll_ready(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
            Poll::Ready(Ok(()))
        }

        fn start_send(self: Pin<&mut Self>, _: JobCommand) -> Result<(), io::Error> {
            Ok(())
        }

        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
            Poll::Ready(Ok(()))
        }

        fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
            Poll::Ready(Ok(()))
        }
    }

    /// Sink that is never ready, parking the first send forever.
    struct StallSink;

    impl Sink<RegisterCommand> for StallSink {
        type Error = io::Error;

        fn poll_ready(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
            Poll::Pending
        }

        fn start_send(self: Pin<&mut Self>, _: RegisterCommand) -> Result<(), io::Error> {
            unreachable!("poll_ready never succeeds")
        }

        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
            Poll::Pending
        }

        fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
            Poll::Pending
        }
    }

    impl Sink<JobCommand> for StallSink {
        type Error = io::Error;

        fn poll_ready(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
            Poll::Pending
        }

        fn start_send(self: Pin<&mut Self>, _: JobCommand) -> Result<(), io::Error> {
            unreachable!("poll_ready never succeeds")
        }

        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
            Poll::Pending
        }

        fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
            Poll::Pending
        }
    }

    /// Peripheral states observed by the tests.
    #[derive(Clone, Default)]
    struct PeripheralFlags {
        reset_asserted: Arc<AtomicBool>,
        rail_disabled: Arc<AtomicBool>,
    }

    struct MockResetLine(PeripheralFlags);

    #[async_trait]
    impl ResetLine for MockResetLine {
        async fn assert(&mut self) -> Result<()> {
            self.0.reset_asserted.store(true, Ordering::SeqCst);
            Ok(())
        }

        async fn release(&mut self) -> Result<()> {
            self.0.reset_asserted.store(false, Ordering::SeqCst);
            Ok(())
        }
    }

    struct MockRegulator(PeripheralFlags);

    #[async_trait]
    impl VoltageRegulator for MockRegulator {
        async fn enable(&mut self) -> Result<()> {
            self.0.rail_disabled.store(false, Ordering::SeqCst);
            Ok(())
        }

        async fn disable(&mut self) -> Result<()> {
            self.0.rail_disabled.store(true, Ordering::SeqCst);
            Ok(())
        }

        async fn is_enabled(&mut self) -> Result<bool> {
            Ok(!self.0.rail_disabled.load(Ordering::SeqCst))
        }

        async fn set_voltage(&mut self, _voltage: Voltage) -> Result<()> {
            Ok(())
        }

        async fn get_voltage(&mut self) -> Result<Voltage> {
            Ok(Voltage::from_volts(1.15))
        }
    }

    fn spawn_thread<R, W>(
        chip_responses: R,
        chip_commands: W,
    ) -> (BM13xxThread, watch::Sender<()>, PeripheralFlags)
    where
        R: Stream<Item = Result<Response, std::io::Error>> + Unpin + Send + 'static,
        W: ChipCommandSink + Unpin + Send + 'static,
        SinkError<W>: std::error::Error + Send + Sync + 'static,
    {
        let sys = Tree::new(Arc::new(Unconfigured)).root();
        spawn_thread_in(
            &sys.child("threads").child("0"),
            chip_responses,
            chip_commands,
        )
    }

    fn spawn_thread_in<R, W>(
        sys: &Cursor,
        chip_responses: R,
        chip_commands: W,
    ) -> (BM13xxThread, watch::Sender<()>, PeripheralFlags)
    where
        R: Stream<Item = Result<Response, std::io::Error>> + Unpin + Send + 'static,
        W: ChipCommandSink + Unpin + Send + 'static,
        SinkError<W>: std::error::Error + Send + Sync + 'static,
    {
        let flags = PeripheralFlags::default();
        let peripherals = BoardPeripherals {
            reset_line: Box::new(MockResetLine(flags.clone())),
            voltage_regulator: Box::new(MockRegulator(flags.clone())),
        };
        let (shutdown_tx, shutdown_rx) = watch::channel(());

        let thread = BM13xxThread::new(
            "test".into(),
            ThreadAttributes::attach(sys),
            chip_config::bm1370(),
            TopologySpec::single_domain(1),
            chip_responses,
            chip_commands,
            peripherals,
            shutdown_rx,
        );

        (thread, shutdown_tx, flags)
    }

    /// Builds a minimal task for driving the actor.
    fn test_task() -> HashTask {
        use crate::asic::bm13xx::test_data::esp_miner_job;
        use crate::job_source::{
            Extranonce2, GeneralPurposeBits, JobTemplate, MerkleRootKind, VersionTemplate,
        };

        let template = Arc::new(JobTemplate {
            id: "test".into(),
            prev_blockhash: *esp_miner_job::wire_tx::PREV_BLOCKHASH,
            version: VersionTemplate::new(
                *esp_miner_job::wire_tx::VERSION,
                GeneralPurposeBits::full(),
            )
            .expect("Valid version template"),
            bits: *esp_miner_job::wire_tx::NBITS,
            share_target: crate::types::Difficulty::from(100_u64).to_target(),
            time: *esp_miner_job::wire_tx::NTIME,
            merkle_root: MerkleRootKind::Fixed(*esp_miner_job::wire_tx::MERKLE_ROOT),
        });
        let (share_tx, _share_rx) = mpsc::channel(1);

        HashTask {
            template,
            en2_range: None,
            en2: Some(Extranonce2::new(0, 1).unwrap()),
            share_target: crate::types::Difficulty::from(100_u64).to_target(),
            ntime: *esp_miner_job::wire_tx::NTIME,
            share_tx,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn exits_when_response_stream_ends() {
        let (mut thread, _shutdown_tx, flags) = spawn_thread(stream::iter(Vec::new()), NullSink);
        let mut events = thread.take_event_receiver().unwrap();

        // The actor disables the chain before it drops its event
        // sender, so a closed event channel implies the cleanup ran
        let closed = time::timeout(Duration::from_secs(5), events.recv()).await;
        assert!(closed.expect("actor should exit").is_none());

        assert!(flags.reset_asserted.load(Ordering::SeqCst));
        assert!(flags.rail_disabled.load(Ordering::SeqCst));
    }

    #[tokio::test(start_paused = true)]
    async fn exits_when_shutdown_sender_drops() {
        let (mut thread, shutdown_tx, flags) = spawn_thread(
            stream::pending::<Result<Response, std::io::Error>>(),
            NullSink,
        );
        let mut events = thread.take_event_receiver().unwrap();

        // Drop the sender without a signal, as a dying board task
        // would
        drop(shutdown_tx);

        let closed = time::timeout(Duration::from_secs(5), events.recv()).await;
        assert!(closed.expect("actor should exit").is_none());

        assert!(flags.reset_asserted.load(Ordering::SeqCst));
        assert!(flags.rail_disabled.load(Ordering::SeqCst));
    }

    #[tokio::test(start_paused = true)]
    async fn exits_on_shutdown_signal() {
        let (_thread, shutdown_tx, flags) = spawn_thread(
            stream::pending::<Result<Response, std::io::Error>>(),
            NullSink,
        );

        shutdown_tx.send(()).unwrap();

        // The actor drops its shutdown receiver only after disabling
        // the chain, so a closed sender implies the cleanup ran
        time::timeout(Duration::from_secs(5), shutdown_tx.closed())
            .await
            .expect("actor should drop its shutdown receiver");

        assert!(flags.reset_asserted.load(Ordering::SeqCst));
        assert!(flags.rail_disabled.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn publishes_a_node_per_chip() {
        let root = Tree::new(Arc::new(Unconfigured)).root();
        let (_thread, _shutdown_tx, _flags) = spawn_thread_in(
            &root.child("threads").child("0"),
            stream::pending::<Result<Response, std::io::Error>>(),
            NullSink,
        );

        assert_eq!(
            root.render_json(),
            Some(serde_json::json!({
                "threads": {"0": {
                    "status": "present",
                    "hashes_per_s": 0,
                    "target_power_fraction": null,
                    "chips": {"0": {
                        "address": 0,
                        "nonces": 0,
                        "hardware_errors": 0,
                        "core_register_reports": 0,
                        "hashes_per_s": 0,
                    }},
                }}
            }))
        );
    }

    #[tokio::test]
    async fn a_zero_power_fraction_holds_the_chain_off() {
        // The stalling sink would park bring-up at its first chip
        // command, so a task that returns proves no bring-up ran
        let (mut thread, _shutdown_tx, flags) = spawn_thread(
            stream::pending::<Result<Response, std::io::Error>>(),
            StallSink,
        );
        thread.set_power_fraction(0.0).await.unwrap();

        let result = time::timeout(Duration::from_secs(5), thread.update_task(test_task()))
            .await
            .expect("a chain held off takes the task at once");

        assert!(result.unwrap().is_none());
        assert!(flags.reset_asserted.load(Ordering::SeqCst));
        assert!(!thread.status().is_active);
    }

    #[tokio::test(start_paused = true)]
    async fn exits_when_handle_dropped() {
        let (mut thread, _shutdown_tx, flags) = spawn_thread(
            stream::pending::<Result<Response, std::io::Error>>(),
            NullSink,
        );
        let mut events = thread.take_event_receiver().unwrap();

        drop(thread);

        let closed = time::timeout(Duration::from_secs(5), events.recv()).await;
        assert!(closed.expect("actor should exit").is_none());

        assert!(flags.reset_asserted.load(Ordering::SeqCst));
        assert!(flags.rail_disabled.load(Ordering::SeqCst));
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_aborts_bring_up() {
        // The stalling sink parks bring-up at its first chip
        // command, holding it in progress until the signal arrives
        let (mut thread, shutdown_tx, flags) = spawn_thread(
            stream::pending::<Result<Response, std::io::Error>>(),
            StallSink,
        );

        let assign = thread.update_task(test_task());
        let signal = async {
            // Bring-up has begun once the startup reset assertion
            // is released
            while !flags.reset_asserted.load(Ordering::SeqCst) {
                time::sleep(Duration::from_millis(1)).await;
            }
            while flags.reset_asserted.load(Ordering::SeqCst) {
                time::sleep(Duration::from_millis(1)).await;
            }
            shutdown_tx.send(()).unwrap();
        };
        let (result, _) = time::timeout(Duration::from_secs(60), async {
            tokio::join!(assign, signal)
        })
        .await
        .expect("bring-up should abort");

        assert!(result.is_err());

        // The actor exits, disabling the chain on the way out
        time::timeout(Duration::from_secs(5), shutdown_tx.closed())
            .await
            .expect("actor should drop its shutdown receiver");
        assert!(flags.reset_asserted.load(Ordering::SeqCst));
        assert!(flags.rail_disabled.load(Ordering::SeqCst));
    }
}
