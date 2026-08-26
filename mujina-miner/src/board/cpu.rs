//! CPU hashboard implementation.
//!
//! Provides a virtual board that uses CPU cores for SHA-256 hashing.
//! See [`CpuMinerConfig`] for environment variable configuration.

use anyhow::{Result, anyhow};
use tokio::sync::watch;

use super::{BackplaneConnector, BoardAttributes, BoardInfo, VirtualBoardDescriptor};
use crate::{
    api_client::types::BoardTelemetry,
    asic::hash_thread::HashThread,
    cpu_miner::{CpuHashThread, CpuMinerConfig},
    systree::Cursor,
};

/// The name this board reports as its model.
const MODEL: &str = "CPU Miner";

inventory::submit! {
    VirtualBoardDescriptor {
        device_type: "cpu_miner",
        name: MODEL,
        create_fn: |boards| Box::pin(create_cpu_board(boards)),
    }
}

async fn create_cpu_board(boards: Cursor) -> Result<BackplaneConnector> {
    let config = CpuMinerConfig::from_env()
        .ok_or_else(|| anyhow!("cpu miner not configured (MUJINA_CPUMINER_THREADS not set)"))?;

    let board_name = format!("cpu-{}x{}%", config.thread_count, config.duty_percent);

    let info = BoardInfo {
        model: MODEL.into(),
        firmware_version: None,
        serial_number: Some(board_name.clone()),
    };

    let attrs = BoardAttributes::attach(&boards.child(&board_name), MODEL);

    let initial_state = BoardTelemetry {
        name: board_name.clone(),
        model: info.model.clone(),
        serial: info.serial_number.clone(),
        ..Default::default()
    };
    let (_telemetry_tx, telemetry_rx) = watch::channel(initial_state);

    let threads: Vec<Box<dyn HashThread>> = (0..config.thread_count)
        .map(|i| {
            Box::new(CpuHashThread::new(
                format!("CPU Core {i}"),
                config.duty_percent,
            )) as _
        })
        .collect();

    Ok(BackplaneConnector {
        info,
        attrs,
        threads,
        telemetry_rx,
        shutdown: None,
    })
}
