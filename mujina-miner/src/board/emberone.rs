//! EmberOne mining board support (stub).
//!
//! The EmberOne is a mining board with 12 BM1362 ASIC chips, communicating via
//! USB using the bitaxe-raw protocol (same as Bitaxe boards).
//!
//! This is currently a stub implementation pending full support.

use async_trait::async_trait;

use super::{
    Board, BoardDescriptor, BoardError, BoardInfo,
    pattern::{BoardPattern, Match, StringMatch},
};
use crate::{asic::hash_thread::HashThread, error::Error, transport::UsbDeviceInfo};

/// EmberOne mining board (stub).
pub struct EmberOne {
    device_info: UsbDeviceInfo,
}

impl EmberOne {
    /// Create a new EmberOne board instance.
    pub fn new(device_info: UsbDeviceInfo) -> Result<Self, BoardError> {
        Ok(Self { device_info })
    }
}

#[async_trait]
impl Board for EmberOne {
    fn board_info(&self) -> BoardInfo {
        BoardInfo {
            model: "EmberOne".to_string(),
            firmware_version: None,
            serial_number: self.device_info.serial_number.clone(),
        }
    }

    async fn shutdown(&mut self) -> Result<(), BoardError> {
        tracing::info!("EmberOne stub shutdown (no-op)");
        Ok(())
    }

    async fn create_hash_threads(&mut self) -> Result<Vec<Box<dyn HashThread>>, BoardError> {
        Err(BoardError::InitializationFailed(
            "EmberOne not yet implemented".into(),
        ))
    }
}

// Factory function to create EmberOne board from USB device info
async fn create_from_usb(device: UsbDeviceInfo) -> crate::error::Result<Box<dyn Board + Send>> {
    let board = EmberOne::new(device)
        .map_err(|e| Error::Hardware(format!("Failed to create board: {}", e)))?;

    Ok(Box::new(board))
}

// Register this board type with the inventory system
inventory::submit! {
    BoardDescriptor {
        pattern: BoardPattern {
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
        let device = UsbDeviceInfo::new_for_test(
            0xc0de,
            0xcafe,
            Some("TEST001".to_string()),
            Some("EmberOne".to_string()),
            Some("Mining Board".to_string()),
            "/sys/devices/test".to_string(),
        );

        let board = EmberOne::new(device);
        assert!(board.is_ok());

        let board = board.unwrap();
        assert_eq!(board.board_info().model, "EmberOne");
    }
}
