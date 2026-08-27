//! Configuration for CPU miner.
//!
//! Parses environment variables to configure the CPU mining backend.

/// CPU miner configuration parsed from environment variables.
///
/// Only whether to mine, and with how many threads, is read here.
/// How hard each thread works is a setting in the tree, so a client
/// can change it while the miner runs.
#[derive(Debug, Clone)]
pub struct CpuMinerConfig {
    /// Number of mining threads to spawn.
    pub thread_count: usize,
}

impl CpuMinerConfig {
    /// Parse configuration from environment variables.
    ///
    /// Returns `Some(config)` if `MUJINA_CPUMINER_THREADS` is set,
    /// `None` otherwise.
    ///
    /// # Environment Variables
    ///
    /// - `MUJINA_CPUMINER_THREADS`: Number of threads (presence enables CPU mining)
    pub fn from_env() -> Option<Self> {
        let thread_count = std::env::var("MUJINA_CPUMINER_THREADS")
            .ok()
            .and_then(|s| s.parse().ok())?;

        Some(Self { thread_count })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    #[serial]
    fn test_from_env_disabled_when_not_set() {
        // SAFETY: Test runs serially, no concurrent env access
        unsafe { std::env::remove_var("MUJINA_CPUMINER_THREADS") };

        let config = CpuMinerConfig::from_env();
        assert!(config.is_none());
    }

    #[test]
    #[serial]
    fn test_thread_count_comes_from_the_environment() {
        // SAFETY: Test runs serially, no concurrent env access
        unsafe { std::env::set_var("MUJINA_CPUMINER_THREADS", "99") };

        let config = CpuMinerConfig::from_env().expect("the variable is set");
        assert_eq!(config.thread_count, 99);
    }
}
