//! HTTP API server.
//!
//! This module implements the REST API server for external control and
//! monitoring of the miner. Built on Axum, it provides endpoints for status,
//! configuration, and real-time updates.
//!
//! The API binds to localhost only by default and does not require
//! authentication for local access.

mod v0;

use anyhow::Result;
use axum::Router;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tower_http::trace::{DefaultMakeSpan, DefaultOnResponse, TraceLayer};
use tracing::{Level, info, warn};

use crate::api_client::types::MinerState;

/// API server configuration.
#[derive(Debug, Clone)]
pub struct ApiConfig {
    /// Address to bind the API server to. Defaults to "127.0.0.1:7785".
    /// Port 7785 represents ASCII 'M' (77) and 'U' (85).
    pub bind_addr: String,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            bind_addr: "127.0.0.1:7785".to_string(),
        }
    }
}

/// Start the API server.
///
/// This function starts the HTTP API server and runs until the provided
/// cancellation token is triggered. It binds to localhost only by default for
/// security.
pub async fn serve(
    config: ApiConfig,
    shutdown: CancellationToken,
    miner_state_rx: watch::Receiver<MinerState>,
) -> Result<()> {
    let app = build_router(miner_state_rx);

    let listener = TcpListener::bind(&config.bind_addr).await?;
    let actual_addr = listener.local_addr()?;

    info!(url = %format!("http://{}", actual_addr), "API server listening.");

    // Warn if binding to non-localhost addresses
    if !actual_addr.ip().is_loopback() {
        warn!(
            "API server is bound to a non-localhost address ({}). \
             This exposes the API to the network without authentication.",
            actual_addr.ip()
        );
    }

    // Run server with graceful shutdown
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown.cancelled().await;
        })
        .await?;

    Ok(())
}

/// Build the application router with all API routes.
fn build_router(miner_state_rx: watch::Receiver<MinerState>) -> Router {
    Router::new()
        .nest("/api/v0", v0::routes())
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::new().level(Level::INFO))
                .on_response(DefaultOnResponse::new().level(Level::INFO)),
        )
        .with_state(miner_state_rx)
}
