//! emberOne/00 hash board support
//!
//! The emberOne/00 has 12 BM1362 ASIC chips, communicating via USB
//! using the bitaxe-raw protocol (same as Bitaxe boards).

use std::{ops::RangeInclusive, sync::Arc, time::Duration};

use crate::tracing::prelude::*;
use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use tokio::sync::{Mutex, watch};
use tokio::task::JoinHandle;
use tokio::time::{self, MissedTickBehavior};
use tokio_serial::SerialPortBuilderExt;
use tokio_util::{
    codec::{FramedRead, FramedWrite},
    sync::CancellationToken,
};

use super::{
    BackplaneConnector, BoardAttributes, BoardDescriptor, BoardInfo, BoardModel,
    pattern::{BoardPattern, Match, StringMatch},
};
use crate::{
    asic::{
        bm13xx::{
            self, chip_config,
            peripherals::{BoardPeripherals, ResetLine},
            register::ChipModel,
            thread::BM13xxThread,
            topology::TopologySpec,
        },
        hash_thread::{HashThread, ThreadAttributes},
    },
    hw_trait::gpio::{Gpio, GpioPin, PinValue},
    mgmt_protocol::{
        ControlChannel,
        bitaxe_raw::{
            DeviceVersion, ResponseFormat,
            gpio::{BitaxeRawGpioController, BitaxeRawGpioPin},
            i2c::BitaxeRawI2c,
            led::BitaxeRawLed,
            system,
        },
    },
    peripheral::{
        led::{CalibratedLed, ColorProfile, Status, StatusLed},
        regulator::RegulatorAttributes,
        tmp451::Tmp451,
        tmp1075::Tmp1075,
        tps546::{PowerStageTuning, Tps546, Tps546Config, Tps546Regulator},
    },
    systree::{Cursor, MemberStatus, Report, WriteGuard},
    transport::{
        UsbDeviceInfo,
        serial::{SerialStream, TracingReader},
    },
    types::{HashRate, Ratio, Temperature, Voltage},
};

/// The model this board reports.
const MODEL: BoardModel = BoardModel {
    name: "emberOne/00",
    key: "emberone-00",
};

/// The range the core rail may be asked for.
///
/// The rail feeds twelve chips in series, so each chip sees a
/// twelfth of it. The top chip's LDOs drop out above 4.20 V across
/// the stack, and 4.0 V keeps margin below that.
const CORE_VOLTAGE_LIMITS: RangeInclusive<Voltage> =
    Voltage::from_volts(3.0)..=Voltage::from_volts(4.0);

/// The rail's set point before a hash thread asks for more, a
/// quarter volt per chip, enough for the chain at its reset
/// frequency.
const INITIAL_CORE_VOLTAGE: Voltage = Voltage::from_volts(3.0);

/// The chips on the chain.
const CHIP_COUNT: usize = 12;

// Register this board type with the inventory system
inventory::submit! {
    BoardDescriptor {
        pattern: BoardPattern {
            vid: Match::Any,
            pid: Match::Any,
            bcd_device: Match::Any,
            manufacturer: Match::Specific(StringMatch::Exact("256F")),
            product: Match::Specific(StringMatch::Exact("EmberOne00")),
            serial_pattern: Match::Any,
        },
        name: MODEL.name,
        create_fn: |device, boards| Box::pin(create_from_usb(device, boards)),
    }
}

/// I2C device addresses on the emberOne/00 board.
mod i2c_addr {
    pub const TMP1075_LEFT: u8 = 0x4A;
    pub const TMP451_RIGHT: u8 = 0x49;
}

/// GPIO command indices exposed by the emberone-usbserial-fw firmware.
///
/// These are not physical pin numbers. The firmware maps each index
/// to the corresponding RP2040 GPIO. See the Command enum in
/// emberone-usbserial-fw/src/control/gpio.rs for the mapping.
mod gpio_cmd {
    /// The chain's reset line, active low. The firmware boots with
    /// it released.
    pub const ASIC_RESET: u8 = 0x00;
    /// The core rail regulator's enable pin.
    pub const PWR_EN: u8 = 0x01;
    /// The boost converter that feeds the chips' IO and clock LDOs.
    pub const VDDIO_EN: u8 = 0x02;
}

/// Select response format based on firmware version.
///
/// Firmware minor >= 1 uses v1 response format with explicit
/// status byte. Older firmware uses v0.
fn response_format(version: &DeviceVersion) -> ResponseFormat {
    if version.firmware_minor() >= 1 {
        ResponseFormat::V1
    } else {
        ResponseFormat::V0
    }
}

async fn create_from_usb(device: UsbDeviceInfo, boards: Cursor) -> Result<BackplaneConnector> {
    let serial_ports = device.get_serial_ports(2).await?;

    debug!(
        serial = ?device.serial_number,
        control = %serial_ports[0],
        data = %serial_ports[1],
        "Opening emberOne/00 serial ports"
    );

    let control_port = tokio_serial::new(&serial_ports[0], 115200)
        .open_native_async()
        .context("failed to open control port")?;
    let version = DeviceVersion::from_bcd(device.bcd_device);
    let format = response_format(&version);
    let control = ControlChannel::new(control_port, format);

    let i2c = BitaxeRawI2c::new(control.clone());

    let data_stream =
        SerialStream::new(&serial_ports[1], 115200).context("failed to open data port")?;
    let (data_reader, data_writer, _data_control) = data_stream.split();
    let data_reader = FramedRead::new(
        TracingReader::new(data_reader, "Data"),
        bm13xx::FrameCodec::new(ChipModel::BM1362),
    );
    let data_writer = FramedWrite::new(data_writer, bm13xx::FrameCodec::new(ChipModel::BM1362));

    let mut gpio = BitaxeRawGpioController::new(control.clone());

    // Hold the chips in reset before any of their rails comes up
    let mut reset_line = EmberOne00ResetLine {
        nrst_pin: gpio.pin(gpio_cmd::ASIC_RESET).await?,
    };
    reset_line.assert().await?;

    let mut vddio_en = gpio.pin(gpio_cmd::VDDIO_EN).await?;
    vddio_en.write(PinValue::High).await?;
    // TPS61041 soft start is ~0.5 ms; MCP1824 LDOs settle in ~0.2 ms
    const VDDIO_SETTLE: Duration = Duration::from_millis(5);
    time::sleep(VDDIO_SETTLE).await;

    let led = BitaxeRawLed::new(control.clone());
    let led = CalibratedLed::new(Box::new(led), ColorProfile::SK6812);
    let status_led = StatusLed::new(Box::new(led), Status::Initializing);

    let mut temp_left = Tmp1075::new(i2c.clone(), i2c_addr::TMP1075_LEFT);
    temp_left
        .init()
        .await
        .context("TMP1075 (left) init failed")?;

    let mut temp_right = Tmp451::new(i2c.clone(), i2c_addr::TMP451_RIGHT);
    temp_right
        .init()
        .await
        .context("TMP451 (right) init failed")?;
    // The remote diode reads high compared to the board sensors
    // at ambient. This offset brings the reading in line at room
    // temperature. Revisit once the ASICs are running to
    // distinguish constant offset from ideality (n-factor) error.
    const REMOTE_OFFSET_C: i8 = -7;
    temp_right
        .set_remote_offset(REMOTE_OFFSET_C)
        .await
        .context("TMP451 remote offset failed")?;

    let regulator = Arc::new(Mutex::new(init_power_controller(i2c.clone()).await?));

    // Enable the regulator once it is configured. Its output stays
    // off until a hash thread commands it on over PMBus.
    let mut pwr_en = gpio.pin(gpio_cmd::PWR_EN).await?;
    pwr_en.write(PinValue::High).await?;

    let info = BoardInfo {
        model: MODEL.name.to_string(),
        firmware_version: None,
        serial_number: device.serial_number.clone(),
    };

    let serial = info
        .serial_number
        .clone()
        .unwrap_or_else(|| "unknown".into());
    let board = BoardAttributes::attach(&boards, MODEL, &serial);
    let attrs = EmberOne00Attributes::attach(board.clone());

    let cancel = CancellationToken::new();
    let monitor_task = EmberOne00Monitor {
        temp_left,
        temp_right,
        regulator: regulator.clone(),
        attrs,
    }
    .spawn(cancel.clone());

    let (thread_shutdown_tx, thread_shutdown_rx) = watch::channel(());
    let thread_name = match &info.serial_number {
        Some(serial) => format!("EmberOne00-{}", &serial[..8.min(serial.len())]),
        None => "EmberOne00".to_string(),
    };

    let peripherals = BoardPeripherals {
        reset_line: Box::new(reset_line.clone()),
        voltage_regulator: Box::new(Tps546Regulator::new(regulator.clone())),
    };

    let chip = chip_config::bm1362();
    let sys_thread = board.cursor().child("threads").child("0");
    let thread_attrs = ThreadAttributes::attach(&sys_thread);
    thread_attrs
        .write()
        .set(&thread_attrs.hashrate, chip.nameplate * CHIP_COUNT);

    // Each chip is its own voltage domain, stacked in series. The
    // BM1362 crosses a domain boundary with no boundary registers
    // written, as the S19j Pro's chain shows.
    let thread = BM13xxThread::new(
        thread_name,
        chip,
        TopologySpec::individual_domains(CHIP_COUNT, false),
        data_reader,
        data_writer,
        peripherals,
        thread_shutdown_rx,
    );
    let threads: Vec<Box<dyn HashThread>> = vec![Box::new(thread)];

    let mut hardware = EmberOne00 {
        control,
        reset_line,
        regulator,
        pwr_en,
        vddio_en,
        status_led,
        monitor_cancel: cancel,
        monitor_task,
        thread_shutdown: thread_shutdown_tx,
    };

    let shutdown = Box::pin(async move {
        hardware.shutdown().await;

        let mut gone = thread_attrs.write();
        gone.set(&thread_attrs.status, MemberStatus::Absent);
        gone.set(&thread_attrs.hashrate, HashRate::default());
    });

    Ok(BackplaneConnector {
        info,
        attrs: board,
        threads,
        shutdown: Some(shutdown),
    })
}

/// What an emberOne/00 holds in the tree.
///
/// It holds what every board owes and adds the three temperatures
/// the board measures and its core rail.
struct EmberOne00Attributes {
    board: BoardAttributes,

    /// The temperature of the left side of the board.
    pcb_left_temperature: Report<Option<Temperature>>,

    /// The temperature of the right side of the board.
    pcb_right_temperature: Report<Option<Temperature>>,

    /// The temperature of the first chip's diode, read through the
    /// sensor on the right side of the board.
    chip_0_temperature: Report<Option<Temperature>>,

    /// The rail that feeds the chain.
    core: RegulatorAttributes,
}

impl EmberOne00Attributes {
    /// Attaches an emberOne/00's own attributes over a board's cursor.
    fn attach(board: BoardAttributes) -> Self {
        let sys = board.cursor().clone();

        Self {
            pcb_left_temperature: sys.report("pcb_left_temperature", None),
            pcb_right_temperature: sys.report("pcb_right_temperature", None),
            chip_0_temperature: sys.report("chip_0_temperature", None),
            core: RegulatorAttributes::attach(
                &sys.child("regulators").child("core"),
                CORE_VOLTAGE_LIMITS,
            ),
            board,
        }
    }

    /// Opens the tree for writing.
    fn write(&self) -> WriteGuard<'_> {
        self.board.write()
    }
}

/// The task that reads the board's sensors and its rail and obeys
/// a client's voltage target.
struct EmberOne00Monitor {
    temp_left: Tmp1075<BitaxeRawI2c>,
    temp_right: Tmp451<BitaxeRawI2c>,
    regulator: Arc<Mutex<Tps546<BitaxeRawI2c>>>,

    /// The board's place in the tree, where the monitor publishes
    /// what it reads and reads what a client asks for.
    attrs: EmberOne00Attributes,
}

impl EmberOne00Monitor {
    fn spawn(self, cancel: CancellationToken) -> JoinHandle<()> {
        tokio::spawn(self.run(cancel))
    }

    async fn run(mut self, cancel: CancellationToken) {
        const INTERVAL: Duration = Duration::from_secs(5);
        let mut ticker = time::interval(INTERVAL);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

        // Discard first tick (fires immediately)
        ticker.tick().await;

        // Subscribe before reading, so a write between the two wakes
        // the loop rather than passing unseen.
        let mut core_voltage = self.attrs.core.target_voltage.subscribe();
        let asked = *core_voltage.borrow_and_update();
        self.apply_core_voltage(asked).await;

        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = ticker.tick() => self.monitor_tick().await,
                Ok(()) = core_voltage.changed() => {
                    let asked = *core_voltage.borrow_and_update();
                    self.apply_core_voltage(asked).await;
                }
            }
        }
    }

    /// Asks the core rail for the voltage a client set.
    ///
    /// An unset target means the daemon decides, and the hash
    /// thread that runs the chain sets the rail as it ramps.
    async fn apply_core_voltage(&mut self, target: Option<Voltage>) {
        let Some(asked) = target else {
            return;
        };
        match self.regulator.lock().await.set_vout_target(asked).await {
            Ok(()) => debug!(%asked, "Core voltage target set"),
            Err(e) => warn!(%asked, error = %e, "Failed to set the core voltage target"),
        }
    }

    /// Reads every sensor once and publishes the pass.
    async fn monitor_tick(&mut self) {
        let t_left = match self.temp_left.read().await {
            Ok(reading) => Some(reading.into()),
            Err(e) => {
                warn!("TMP1075 (left) read failed: {}", e);
                None
            }
        };

        let t_right = match self.temp_right.read_local().await {
            Ok(reading) => Some(reading.into()),
            Err(e) => {
                warn!("TMP451 (right) local read failed: {}", e);
                None
            }
        };

        // TODO: attribute to the chip's hash thread once implemented
        let t_chip0 = match self.temp_right.read_remote().await {
            Ok(reading) => Some(reading.into()),
            Err(e) => {
                if !matches!(e, crate::peripheral::tmp451::Error::RemoteDiodeOpen) {
                    warn!("TMP451 remote read failed: {}", e);
                }
                None
            }
        };

        let (vin, vout, iout_ma, power_mw, vr_temp) = {
            let mut reg = self.regulator.lock().await;

            if let Err(e) = reg.check_status().await {
                error!("Power controller fault: {}", e);
                if let Err(e) = reg.clear_faults().await {
                    error!("Failed to clear faults: {}", e);
                }
            }

            (
                reg.get_vin().await.ok(),
                reg.get_vout().await.ok(),
                reg.get_iout().await.ok(),
                reg.get_power().await.ok(),
                reg.get_temperature().await.ok(),
            )
        };

        // Publish the whole pass under one guard, so a render
        // never mixes this pass with the one before it. A failed
        // read publishes null rather than leaving the last good
        // reading to look current.
        let attrs = &self.attrs;
        let mut published = attrs.write();
        published.set(&attrs.pcb_left_temperature, t_left);
        published.set(&attrs.pcb_right_temperature, t_right);
        published.set(&attrs.chip_0_temperature, t_chip0);
        published.set(&attrs.core.input_voltage, vin);
        published.set(&attrs.core.voltage, vout);
        published.set(&attrs.core.current, iout_ma.map(|ma| ma as f32 / 1000.0));
        published.set(&attrs.core.power, power_mw.map(|mw| mw as f32 / 1000.0));
        published.set(
            &attrs.core.temperature,
            vr_temp.map(|c| Temperature::from_celsius(c as f32)),
        );
    }
}

/// Configures the core rail's regulator and leaves its output off.
async fn init_power_controller(i2c: BitaxeRawI2c) -> Result<Tps546<BitaxeRawI2c>> {
    let config = Tps546Config {
        phase: 0xFF,
        frequency_switch_khz: 325,

        vin_on: Voltage::from_volts(11.0),
        vin_off: Voltage::from_volts(10.5),
        vin_uv_warn_limit: Voltage::from_volts(0.0), // Disabled due to TI bug
        // Above 13.0 V a normal transient on a 12 V input trips the
        // fault and drops the output
        vin_ov_fault_limit: Voltage::from_volts(14.0),
        vin_ov_fault_response: 0xB7,

        vout_scale_loop: 0.125,
        vout_min: *CORE_VOLTAGE_LIMITS.start(),
        vout_max: *CORE_VOLTAGE_LIMITS.end(),
        vout_command: INITIAL_CORE_VOLTAGE,

        vout_ov_fault_limit: Ratio::from_factor(1.25),
        vout_ov_warn_limit: Ratio::from_factor(1.16),
        vout_margin_high: Ratio::from_factor(1.10),
        vout_margin_low: Ratio::from_factor(0.90),
        vout_uv_warn_limit: Ratio::from_factor(0.90),
        vout_uv_fault_limit: Ratio::from_factor(0.75),
        vout_ov_fault_response: Some(0xB7),
        vout_uv_fault_response: Some(0xB7),

        iout_oc_warn_limit: 50.0,
        iout_oc_fault_limit: 55.0,
        iout_oc_fault_response: 0xC0,

        ot_warn_limit: 105,
        ot_fault_limit: 145,
        ot_fault_response: 0xFF,

        ton_delay: 0,
        ton_rise: 3,
        ton_max_fault_limit: 0,
        ton_max_fault_response: 0x3B,
        toff_delay: 0,
        toff_fall: 0,

        // Take the pin-strapped values for everything not written
        // below
        pin_detect_override: 0x0000,

        // Loop coefficients from TI's compensation calculator for
        // the 740 nH inductor and the output capacitors on this
        // board
        power_stage: Some(PowerStageTuning {
            compensation: [0x13, 0x20, 0xC6, 0x19, 0xC6],
            power_stage: 0x70,
            telemetry: [0x03, 0x03, 0x03, 0x03, 0x03, 0x00],
            iout_cal_gain: 0xC880,
            iout_cal_offset: 0xE000,
        }),
    };

    let mut tps546 = Tps546::new(i2c, config);
    tps546
        .init()
        .await
        .context("power controller init failed")?;
    tps546
        .clear_faults()
        .await
        .context("failed to clear faults")?;

    if let Err(e) = tps546.dump_configuration().await {
        warn!("Failed to dump TPS546 configuration: {}", e);
    }

    Ok(tps546)
}

/// The chain's reset line, driven through the firmware's GPIO
/// command.
#[derive(Clone)]
struct EmberOne00ResetLine {
    nrst_pin: BitaxeRawGpioPin,
}

#[async_trait]
impl ResetLine for EmberOne00ResetLine {
    async fn assert(&mut self) -> Result<()> {
        self.nrst_pin
            .write(PinValue::Low)
            .await
            .map_err(|e| anyhow!("failed to assert reset: {}", e))
    }

    async fn release(&mut self) -> Result<()> {
        self.nrst_pin
            .write(PinValue::High)
            .await
            .map_err(|e| anyhow!("failed to release reset: {}", e))
    }
}

/// emberOne/00 hash board state.
///
/// Holds hardware handles needed for the board's lifetime.
/// Moved into the shutdown future by the factory function.
struct EmberOne00 {
    control: ControlChannel,
    reset_line: EmberOne00ResetLine,
    regulator: Arc<Mutex<Tps546<BitaxeRawI2c>>>,
    pwr_en: BitaxeRawGpioPin,
    vddio_en: BitaxeRawGpioPin,
    status_led: StatusLed,
    monitor_cancel: CancellationToken,
    monitor_task: JoinHandle<()>,

    /// Shutdown signal to the hash thread. A send requests
    /// shutdown; the thread's exit drops its receiver.
    thread_shutdown: watch::Sender<()>,
}

impl EmberOne00 {
    /// Powers the board down in the reverse of its bring-up order.
    async fn shutdown(&mut self) {
        self.monitor_cancel.cancel();
        let _ = (&mut self.monitor_task).await;

        // The thread drops its shutdown receiver on exit, after
        // disabling the chain, so closed() means it has finished.
        // A failed send means it is already gone.
        const THREAD_EXIT_TIMEOUT: Duration = Duration::from_secs(2);
        let _ = self.thread_shutdown.send(());
        if time::timeout(THREAD_EXIT_TIMEOUT, self.thread_shutdown.closed())
            .await
            .is_err()
        {
            warn!("Timed out waiting for thread to exit");
        }

        if let Err(e) = self.reset_line.assert().await {
            warn!("Failed to hold chips in reset: {}", e);
        }
        match self.regulator.lock().await.disable_output().await {
            Ok(()) => debug!("Core voltage turned off"),
            Err(e) => warn!("Failed to disable output: {}", e),
        }
        let _ = self.pwr_en.write(PinValue::Low).await;
        let _ = self.vddio_en.write(PinValue::Low).await;
        self.status_led.off().await;
        let _ = system::reboot(&self.control).await;
    }
}

#[cfg(test)]
mod tests {}

#[cfg(all(test, target_os = "linux"))]
mod integration_tests;
