//! CPU hashboard implementation.
//!
//! Provides a virtual board that uses CPU cores for SHA-256 hashing.
//! See [`CpuMinerConfig`] for environment variable configuration.

use anyhow::{Result, anyhow};
use tokio::sync::watch;

use super::{BackplaneConnector, BoardAttributes, BoardInfo, BoardModel, VirtualBoardDescriptor};
use crate::{
    api_client::types::BoardTelemetry,
    asic::hash_thread::HashThread,
    cpu_miner::{CpuHashThread, CpuMinerConfig},
    systree::Cursor,
};

/// The model this board reports.
const MODEL: BoardModel = BoardModel {
    name: "CPU Miner",
    key: "cpu-miner",
};

/// The serial this board reports.
///
/// One daemon runs one CPU board, so its identity never varies. A
/// serial made from the thread count would move the board's saved
/// settings whenever the count changed.
const SERIAL: &str = "0";

inventory::submit! {
    VirtualBoardDescriptor {
        device_type: "cpu_miner",
        name: MODEL.name,
        create_fn: |boards| Box::pin(create_cpu_board(boards)),
    }
}

async fn create_cpu_board(boards: Cursor) -> Result<BackplaneConnector> {
    let config = CpuMinerConfig::from_env()
        .ok_or_else(|| anyhow!("cpu miner not configured (MUJINA_CPUMINER_THREADS not set)"))?;

    let board_name = format!("cpu-{}x{}%", config.thread_count, config.duty_percent);

    let info = BoardInfo {
        model: MODEL.name.into(),
        firmware_version: None,
        serial_number: Some(SERIAL.into()),
    };

    let attrs = BoardAttributes::attach(&boards, MODEL, SERIAL);

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
