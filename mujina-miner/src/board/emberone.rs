//! EmberOne mining board support.
//!
//! The EmberOne is a mining board with 12 BM1362 ASIC chips, communicating via
//! USB using the bitaxe-raw protocol (same as Bitaxe boards).
//!
//! ## Current Implementation Status
//!
//! This is currently a stub implementation that:
//! - Gets discovered via USB (VID 0xc0de, PID 0xcafe)
//! - Logs when connected
//! - Returns errors for all operations (not yet implemented)
//!
//! ## Future Implementation
//!
//! Will support:
//! - 12 BM1362 ASIC chips in a chain configuration
//! - Dual serial ports (control + data)
//! - bitaxe-raw management protocol
//! - Temperature and power monitoring
//! - Full mining operations

use async_trait::async_trait;
use tokio::sync::mpsc;

use super::{
    pattern::{Match, StringMatch},
    Board, BoardError, BoardEvent, BoardInfo,
};
use crate::{
    asic::{ChipInfo, MiningJob},
    hash_thread::HashThread,
    transport::UsbDeviceInfo,
};

/// EmberOne mining board (stub implementation).
pub struct EmberOne {
    device_info: UsbDeviceInfo,
}

impl EmberOne {
    /// Create a new EmberOne board instance.
    pub fn new(device_info: UsbDeviceInfo) -> Result<Self, BoardError> {
        tracing::info!(
            "EmberOne stub created for device {:04x}:{:04x}",
            device_info.vid,
            device_info.pid
        );
        Ok(Self { device_info })
    }
}

#[async_trait]
impl Board for EmberOne {
    async fn reset(&mut self) -> Result<(), BoardError> {
        Err(BoardError::InitializationFailed(
            "EmberOne reset not yet implemented".into(),
        ))
    }

    async fn hold_in_reset(&mut self) -> Result<(), BoardError> {
        Err(BoardError::InitializationFailed(
            "EmberOne hold_in_reset not yet implemented".into(),
        ))
    }

    async fn initialize(&mut self) -> Result<mpsc::Receiver<BoardEvent>, BoardError> {
        Err(BoardError::InitializationFailed(
            "EmberOne initialize not yet implemented".into(),
        ))
    }

    fn chip_count(&self) -> usize {
        // EmberOne has 12 BM1362 chips, but not yet implemented
        0
    }

    fn chip_infos(&self) -> &[ChipInfo] {
        // No chips discovered yet in stub
        &[]
    }

    async fn send_job(&mut self, _job: &MiningJob) -> Result<(), BoardError> {
        Err(BoardError::InitializationFailed(
            "EmberOne send_job not yet implemented".into(),
        ))
    }

    async fn cancel_job(&mut self, _job_id: u64) -> Result<(), BoardError> {
        Err(BoardError::InitializationFailed(
            "EmberOne cancel_job not yet implemented".into(),
        ))
    }

    fn board_info(&self) -> BoardInfo {
        BoardInfo {
            model: "EmberOne (stub)".to_string(),
            firmware_version: None,
            serial_number: self.device_info.serial_number.clone(),
        }
    }

    fn take_event_receiver(&mut self) -> Option<mpsc::Receiver<BoardEvent>> {
        // Stub has no event receiver yet
        None
    }

    async fn shutdown(&mut self) -> Result<(), BoardError> {
        tracing::info!("EmberOne stub shutdown (no-op)");
        Ok(())
    }

    async fn create_hash_threads(&mut self) -> Result<Vec<Box<dyn HashThread>>, BoardError> {
        Err(BoardError::InitializationFailed(
            "EmberOne hash threads not yet implemented".into(),
        ))
    }
}

// Factory function to create EmberOne board from USB device info
async fn create_from_usb(device: UsbDeviceInfo) -> crate::error::Result<Box<dyn Board + Send>> {
    tracing::info!(
        "Detected EmberOne board (stub): VID={:04x} PID={:04x} Serial={:?}",
        device.vid,
        device.pid,
        device.serial_number
    );

    // EmberOne uses 2 serial ports like Bitaxe (control + data)
    if device.serial_ports.len() != 2 {
        tracing::warn!(
            "EmberOne expected 2 serial ports, found {}. Creating stub anyway.",
            device.serial_ports.len()
        );
    } else {
        tracing::info!(
            "EmberOne serial ports: control={}, data={}",
            device.serial_ports[0],
            device.serial_ports[1]
        );
    }

    let board = EmberOne::new(device)
        .map_err(|e| crate::error::Error::Hardware(format!("Failed to create board: {}", e)))?;

    tracing::info!("EmberOne stub board created successfully");

    Ok(Box::new(board))
}

// Register this board type with the inventory system
inventory::submit! {
    crate::board::BoardDescriptor {
        pattern: crate::board::pattern::BoardPattern {
            vid: Match::Any,
            pid: Match::Any,
            manufacturer: Match::Specific(StringMatch::Exact("256F")),
            product: Match::Specific(StringMatch::Exact("EmberOne00")),
            serial_pattern: Match::Any,
        },
        name: "EmberOne",
        create_fn: |device| Box::pin(create_from_usb(device)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_board_creation() {
        let device = UsbDeviceInfo {
            vid: 0xc0de,
            pid: 0xcafe,
            serial_number: Some("TEST001".to_string()),
            manufacturer: Some("EmberOne".to_string()),
            product: Some("Mining Board".to_string()),
            device_path: "/sys/devices/test".to_string(),
            serial_ports: vec!["/dev/ttyACM0".to_string(), "/dev/ttyACM1".to_string()],
        };

        let board = EmberOne::new(device);
        assert!(board.is_ok());

        let board = board.unwrap();
        assert_eq!(board.chip_count(), 0); // Stub returns 0 chips
        assert_eq!(board.board_info().model, "EmberOne (stub)");
    }
}
