//! CPU hashboard implementation.
//!
//! Provides a virtual board that uses CPU cores for SHA-256 hashing.
//! See [`CpuMinerConfig`] for environment variable configuration.

use anyhow::{Result, anyhow};

use super::{BackplaneConnector, BoardAttributes, BoardInfo, BoardModel, VirtualBoardDescriptor};
use crate::{
    asic::hash_thread::HashThread,
    cpu_miner::{CpuHashThread, CpuMinerConfig, CpuThreadAttributes},
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

    let info = BoardInfo {
        model: MODEL.name.into(),
        firmware_version: None,
        serial_number: Some(SERIAL.into()),
    };

    let attrs = BoardAttributes::attach(&boards, MODEL, SERIAL);

    let sys_threads = attrs.cursor().child("threads");
    let threads: Vec<Box<dyn HashThread>> = (0..config.thread_count)
        .map(|i| {
            let thread = CpuThreadAttributes::attach(&sys_threads.child(&i.to_string()));
            Box::new(CpuHashThread::new(format!("CPU Core {i}"), thread)) as _
        })
        .collect();

    Ok(BackplaneConnector {
        info,
        attrs,
        threads,
        shutdown: None,
    })
}
