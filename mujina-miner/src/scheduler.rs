//! The scheduler module manages the distribution of mining jobs to hash boards
//! and ASIC chips.
//!
//! This is a work-in-progress. It's currently the main and initial place where
//! functionality is added, after which the functionality is refactored out to
//! where it belongs.

use std::time::Duration;
use tokio_serial::{self, SerialPortBuilderExt};
use tokio_util::sync::CancellationToken;

use crate::board::{bitaxe::BitaxeBoard, Board, BoardEvent, BoardError};
use crate::chip::bm13xx::protocol::{BM13xxProtocol, ChipType, Frequency};
use crate::job_generator::JobGenerator;
use crate::tracing::prelude::*;

const CONTROL_SERIAL: &str = "/dev/ttyACM0";
const DATA_SERIAL: &str = "/dev/ttyACM1";

/// Initial mining frequency in MHz (start conservative)
const INITIAL_FREQUENCY_MHZ: f32 = 200.0;
/// Target mining frequency in MHz (can be ramped up to)
const TARGET_FREQUENCY_MHZ: f32 = 500.0;
/// Frequency ramp step size in MHz
const FREQUENCY_STEP_MHZ: f32 = 25.0;
/// Delay between frequency steps to allow chip stabilization
const FREQUENCY_STEP_DELAY_MS: u64 = 500;

// TODO: Future enhancements for frequency ramping:
// - Make ramp parameters configurable (step size, delay, target)
// - Monitor chip temperature/errors during ramp
// - Coordinate with board-level voltage regulators
// - Implement adaptive ramping based on chip response
// - Add rollback on errors during ramp

pub async fn task(running: CancellationToken) {
    trace!("Scheduler task started.");

    // In the future, a DeviceManager would create boards based on USB detection
    // For now, we'll create a single board with known serial ports
    let control_port = tokio_serial::new(CONTROL_SERIAL, 115200)
        .open_native_async()
        .expect("failed to open control serial port");
    
    let data_port = tokio_serial::new(DATA_SERIAL, 115200)
        .open_native_async()
        .expect("failed to open data serial port");
    
    let mut board = BitaxeBoard::new(control_port, data_port);
    
    // Initialize the board (reset + chip discovery)
    let mut event_rx = match board.initialize().await {
        Ok(rx) => {
            info!("Board initialized successfully");
            info!("Found {} chip(s)", board.chip_count());
            rx
        }
        Err(e) => {
            error!("Failed to initialize board: {e}");
            return;
        }
    };
    
    // Configure chips for mining
    if let Err(e) = configure_chips_for_mining(&mut board).await {
        error!("Failed to configure chips: {e}");
        return;
    }
    
    // Create job generator for testing (using difficulty 1 for easy verification)
    let mut job_generator = JobGenerator::new(1.0);
    info!("Created job generator with difficulty 1.0");
    
    // Send initial job to start mining
    let initial_job = job_generator.next_job();
    if let Err(e) = board.send_job(&initial_job).await {
        error!("Failed to send initial job: {e}");
        return;
    }
    info!("Sent initial mining job to chips");
    
    // Main scheduler loop
    info!("Starting mining scheduler");
    
    while !running.is_cancelled() {
        tokio::select! {
            // Handle board events
            Some(event) = event_rx.recv() => {
                match event {
                    BoardEvent::NonceFound(nonce_result) => {
                        info!("Nonce found! Job {} nonce {:#x}", nonce_result.job_id, nonce_result.nonce);
                        // TODO: Submit to pool
                    }
                    BoardEvent::JobComplete { job_id, reason } => {
                        info!("Job {} completed: {:?}", job_id, reason);
                        
                        // Send a new job to keep the chips busy
                        let new_job = job_generator.next_job();
                        if let Err(e) = board.send_job(&new_job).await {
                            error!("Failed to send new job: {e}");
                        } else {
                            debug!("Sent new job {} to chips", new_job.job_id);
                        }
                    }
                    BoardEvent::ChipError { chip_address, error } => {
                        error!("Chip {} error: {}", chip_address, error);
                    }
                    BoardEvent::ChipStatusUpdate { chip_address, temperature_c, frequency_mhz } => {
                        trace!("Chip {} status - temp: {:?}°C, freq: {:?}MHz", 
                               chip_address, temperature_c, frequency_mhz);
                    }
                }
            }
            
            // Periodic status check
            _ = tokio::time::sleep(tokio::time::Duration::from_secs(30)) => {
                trace!("Scheduler heartbeat - mining active");
            }
            
            // Shutdown
            _ = running.cancelled() => {
                info!("Scheduler shutdown requested");
                break;
            }
        }
    }
    
    trace!("Scheduler task stopped.");
}

/// Configure discovered chips for mining operation.
/// 
/// This includes:
/// - Setting initial PLL frequency (with ramping)
/// - Enabling version rolling
/// - Configuring other chip-specific settings
async fn configure_chips_for_mining(board: &mut BitaxeBoard) -> Result<(), BoardError> {
    info!("Configuring chips for mining...");
    
    // Get chip info to determine chip type
    let chip_infos = board.chip_infos();
    if chip_infos.is_empty() {
        return Err(BoardError::InitializationFailed("No chips discovered".to_string()));
    }
    
    // Check what type of chips we have
    let chip_type = ChipType::from(chip_infos[0].chip_id);
    info!("Detected chip type: {:?}", chip_type);
    
    // Create protocol handler
    let protocol = BM13xxProtocol::new(true); // Enable version rolling
    
    // Get initialization commands for single chip (Bitaxe has one chip)
    let init_freq = Frequency::from_mhz(INITIAL_FREQUENCY_MHZ)
        .map_err(|e| BoardError::InitializationFailed(format!("Invalid frequency: {}", e)))?;
    
    let init_commands = protocol.single_chip_init(init_freq);
    
    // Send initialization commands
    info!("Sending {} initialization commands", init_commands.len());
    board.send_config_commands(init_commands).await?;
    
    // Wait for chip to stabilize at initial frequency
    tokio::time::sleep(Duration::from_millis(FREQUENCY_STEP_DELAY_MS)).await;
    
    // Perform frequency ramping if needed
    if TARGET_FREQUENCY_MHZ > INITIAL_FREQUENCY_MHZ {
        info!("Starting frequency ramp from {} MHz to {} MHz", 
              INITIAL_FREQUENCY_MHZ, TARGET_FREQUENCY_MHZ);
        
        let mut current_freq = INITIAL_FREQUENCY_MHZ;
        while current_freq < TARGET_FREQUENCY_MHZ {
            current_freq = (current_freq + FREQUENCY_STEP_MHZ).min(TARGET_FREQUENCY_MHZ);
            
            let freq = Frequency::from_mhz(current_freq)
                .map_err(|e| BoardError::InitializationFailed(format!("Invalid frequency: {}", e)))?;
            
            // Generate PLL commands for new frequency
            let pll_commands = protocol.frequency_ramp(
                Frequency::from_mhz(current_freq - FREQUENCY_STEP_MHZ).unwrap(),
                freq,
                1  // Single step since we're doing it manually
            );
            
            info!("Setting frequency to {} MHz", current_freq);
            board.send_config_commands(pll_commands).await?;
            
            // Wait for chip to stabilize
            tokio::time::sleep(Duration::from_millis(FREQUENCY_STEP_DELAY_MS)).await;
        }
        
        info!("Frequency ramp complete");
    }
    
    info!("Chip configuration complete");
    Ok(())
}