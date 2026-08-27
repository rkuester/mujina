//! emberOne/00 hash board support
//!
//! The emberOne/00 has 12 BM1362 ASIC chips, communicating via USB
//! using the bitaxe-raw protocol (same as Bitaxe boards).

use std::time::Duration;

use crate::tracing::prelude::*;
use anyhow::{Context, Result};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::{self, MissedTickBehavior};
use tokio_serial::SerialPortBuilderExt;
use tokio_util::sync::CancellationToken;

use super::{
    BackplaneConnector, BoardAttributes, BoardDescriptor, BoardInfo, BoardModel,
    pattern::{BoardPattern, Match, StringMatch},
};
use crate::{
    api_client::types::{BoardTelemetry, TemperatureSensor},
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
        tmp451::Tmp451,
        tmp1075::Tmp1075,
    },
    systree::{Cursor, Report, WriteGuard},
    transport::UsbDeviceInfo,
    types::Temperature,
};

/// The model this board reports.
const MODEL: BoardModel = BoardModel {
    name: "emberOne/00",
    key: "emberone-00",
};

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

    let mut gpio = BitaxeRawGpioController::new(control.clone());
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

    let info = BoardInfo {
        model: MODEL.name.to_string(),
        firmware_version: None,
        serial_number: device.serial_number.clone(),
    };

    let serial = info
        .serial_number
        .clone()
        .unwrap_or_else(|| "unknown".into());
    // The board telemetry document keeps the name it has always
    // used, which is not the board's key in the tree.
    let board_name = format!("emberone00-{serial}");
    let board = BoardAttributes::attach(&boards, MODEL, &serial);
    let attrs = EmberOne00Attributes::attach(board.clone());

    let initial_telemetry = BoardTelemetry {
        name: board_name,
        model: info.model.clone(),
        serial: info.serial_number.clone(),
        ..Default::default()
    };
    let (telemetry_tx, telemetry_rx) = watch::channel(initial_telemetry);

    let cancel = CancellationToken::new();
    let monitor_task = spawn_monitor(temp_left, temp_right, attrs, telemetry_tx, cancel.clone());

    let mut hardware = EmberOne00 {
        control,
        vddio_en,
        status_led,
        monitor_cancel: cancel,
        monitor_task,
    };

    let shutdown = Box::pin(async move {
        hardware.shutdown().await;
    });

    warn!("emberOne/00 hash threads not yet implemented");

    Ok(BackplaneConnector {
        info,
        attrs: board,
        threads: Vec::new(),
        telemetry_rx,
        shutdown: Some(shutdown),
    })
}

/// What an emberOne/00 holds in the tree.
///
/// It holds what every board owes and adds the three temperatures
/// the board measures.
struct EmberOne00Attributes {
    board: BoardAttributes,

    /// The temperature of the left side of the board.
    pcb_left_temperature: Report<Option<Temperature>>,

    /// The temperature of the right side of the board.
    pcb_right_temperature: Report<Option<Temperature>>,

    /// The temperature of the first chip's diode, read through the
    /// sensor on the right side of the board.
    chip_0_temperature: Report<Option<Temperature>>,
}

impl EmberOne00Attributes {
    /// Attaches an emberOne/00's own attributes over a board's cursor.
    fn attach(board: BoardAttributes) -> Self {
        let sys = board.cursor().clone();

        Self {
            pcb_left_temperature: sys.report("pcb_left_temperature", None),
            pcb_right_temperature: sys.report("pcb_right_temperature", None),
            chip_0_temperature: sys.report("chip_0_temperature", None),
            board,
        }
    }

    /// Opens the tree for writing.
    fn write(&self) -> WriteGuard<'_> {
        self.board.write()
    }
}

/// Spawn a task that periodically reads the board's sensors.
fn spawn_monitor(
    mut temp_left: Tmp1075<BitaxeRawI2c>,
    mut temp_right: Tmp451<BitaxeRawI2c>,
    attrs: EmberOne00Attributes,
    telemetry_tx: watch::Sender<BoardTelemetry>,
    cancel: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        const INTERVAL: Duration = Duration::from_secs(5);
        let mut ticker = time::interval(INTERVAL);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

        // Discard first tick (fires immediately)
        ticker.tick().await;

        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = ticker.tick() => {}
            }

            let t_left = match temp_left.read().await {
                Ok(reading) => Some(reading.into()),
                Err(e) => {
                    warn!("TMP1075 (left) read failed: {}", e);
                    None
                }
            };

            let t_right = match temp_right.read_local().await {
                Ok(reading) => Some(reading.into()),
                Err(e) => {
                    warn!("TMP451 (right) local read failed: {}", e);
                    None
                }
            };

            // TODO: attribute to the chip's hash thread once implemented
            let t_chip0 = match temp_right.read_remote().await {
                Ok(reading) => Some(reading.into()),
                Err(e) => {
                    if !matches!(e, crate::peripheral::tmp451::Error::RemoteDiodeOpen) {
                        warn!("TMP451 remote read failed: {}", e);
                    }
                    None
                }
            };

            // Publish the whole pass under one guard, so a render
            // never mixes this pass with the one before it.
            {
                let mut published = attrs.write();
                published.set(&attrs.pcb_left_temperature, t_left);
                published.set(&attrs.pcb_right_temperature, t_right);
                published.set(&attrs.chip_0_temperature, t_chip0);
            }

            telemetry_tx.send_modify(|t| {
                t.temperatures = vec![
                    TemperatureSensor {
                        name: "pcb-left".into(),
                        temperature: t_left,
                    },
                    TemperatureSensor {
                        name: "pcb-right".into(),
                        temperature: t_right,
                    },
                    TemperatureSensor {
                        name: "chip-0".into(),
                        temperature: t_chip0,
                    },
                ];
            });
        }
    })
}

/// emberOne/00 hash board state.
///
/// Holds hardware handles needed for the board's lifetime.
/// Moved into the shutdown future by the factory function.
struct EmberOne00 {
    control: ControlChannel,
    vddio_en: BitaxeRawGpioPin,
    status_led: StatusLed,
    monitor_cancel: CancellationToken,
    monitor_task: JoinHandle<()>,
}

impl EmberOne00 {
    async fn shutdown(&mut self) {
        self.monitor_cancel.cancel();
        let _ = (&mut self.monitor_task).await;
        let _ = self.vddio_en.write(PinValue::Low).await;
        self.status_led.off().await;
        let _ = system::reboot(&self.control).await;
    }
}

#[cfg(test)]
mod tests {}

#[cfg(all(test, target_os = "linux"))]
mod integration_tests;
