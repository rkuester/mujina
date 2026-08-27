//! CPU mining backend.
//!
//! Provides a virtual mining board that uses CPU cores for SHA-256 hashing.
//! Useful for testing and development without physical ASIC hardware.
//!
//! # Configuration
//!
//! Enable via environment variables:
//!
//! - `MUJINA_CPUMINER_THREADS=N` - Number of mining threads (presence enables)
//!
//! The duty cycle is a setting in the tree, at each thread's
//! `target_duty_percent`, which `MUJINA_CPUMINER_DUTY` sets through
//! the environment layer of the configuration cascade.

mod attributes;
mod config;
mod hasher;
mod thread;

pub use attributes::{CpuThreadAttributes, DEFAULT_DUTY};
pub use config::CpuMinerConfig;
pub use thread::CpuHashThread;
