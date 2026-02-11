//! API client library.
//!
//! Provides a Rust client for the miner's HTTP API, shared by the CLI
//! and TUI binaries.

pub mod types;

use anyhow::{Context, Result};
use reqwest::Client as HttpClient;

use types::MinerState;

/// Default API base URL.
///
/// Port 7785 = ASCII 'M' (77) + 'U' (85).
const DEFAULT_BASE_URL: &str = "http://127.0.0.1:7785";

/// HTTP client for the miner API.
pub struct Client {
    http: HttpClient,
    base_url: String,
}

impl Client {
    /// Create a client connecting to the default local address.
    pub fn new() -> Self {
        Self {
            http: HttpClient::new(),
            base_url: DEFAULT_BASE_URL.to_string(),
        }
    }

    /// Create a client connecting to a specific base URL.
    pub fn with_base_url(base_url: String) -> Self {
        Self {
            http: HttpClient::new(),
            base_url,
        }
    }

    /// Fetch the current miner state snapshot.
    pub async fn get_miner(&self) -> Result<MinerState> {
        let url = format!("{}/api/v0/miner", self.base_url);
        let response = self
            .http
            .get(&url)
            .send()
            .await
            .context("failed to connect to miner API")?;
        let status = response.status();
        if !status.is_success() {
            anyhow::bail!("API request failed: {}", status);
        }
        response
            .json()
            .await
            .context("failed to parse miner state response")
    }
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}
