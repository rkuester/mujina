//! The scheduler module manages the distribution of mining jobs to hash boards
//! and ASIC chips.
//!
//! This is a work-in-progress. It's currently the main and initial place where
//! functionality is added, after which the functionality is refactored out to
//! where it belongs.

use futures::sink::SinkExt;
use tokio::time::{self, Duration};
use tokio_serial::{self, SerialPortBuilderExt};
use tokio_stream::StreamExt;
use tokio_util::codec::{FramedRead, FramedWrite};
use tokio_util::sync::CancellationToken;

use crate::chip::bm13xx;
use crate::board::bitaxe;
use crate::tracing::prelude::*;

pub async fn task(running: CancellationToken) {
    trace!("Task started.");

    let stream = tokio_serial::new(bitaxe::DATA_SERIAL, 115200)
        .open_native_async()
        .expect("failed to open data serial port");
    let (read_stream, write_stream) = tokio::io::split(stream);
    let mut framed_read = FramedRead::new(read_stream, bm13xx::FrameCodec::default());
    let mut framed_write = FramedWrite::new(write_stream, bm13xx::FrameCodec::default());

    let read_task = {
        let running = running.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    response = framed_read.next() => {
                        match response {
                            Some(Ok(bm13xx::Response::ReadRegister {
                                chip_address,
                                register: bm13xx::Register::ChipAddress {
                                    chip_id, core_count, address
                                }
                            })) => {
                                info!("discovered chip 0x{chip_id:x}@{address}")
                            },
                            Some(Err(e)) => {
                                error!("Error reading from port: {e}");
                            },
                            None => {
                                warn!("Stream ended");
                                break;
                            },
                            _ => {},
                        }
                    }
                    _ = running.cancelled() => {
                        trace!("Read task stopped");
                        break;
                    }
                }
            }
        })
    };

    bitaxe::deassert_reset().await;

    let write_task = tokio::spawn(async move {
        while !running.is_cancelled() {
            let command = bm13xx::Command::ReadRegister {
                all: true,
                chip_address: 0,
                register_address: bm13xx::RegisterAddress::ChipAddress,
            };

            trace!("Writing to port.");
            if let Err(e) = framed_write.send(command).await {
                error!("Error {e} writing to port.");
                break;
            }

            // Sleep to avoid busy loop
            tokio::select! {
                _ = time::sleep(Duration::from_secs(1)) => {},
                _ = running.cancelled() => break,
            }
        }
        trace!("Write task stopped");
    });

    // Wait for both tasks to complete
    let (read_result, write_result) = tokio::join!(read_task, write_task);
    
    if let Err(e) = read_result {
        error!("Read task panicked: {e}");
    }
    if let Err(e) = write_result {
        error!("Write task panicked: {e}");
    }
    
    trace!("Tasks completed");
}
