//! HTTP server lifecycle and router construction.

use std::sync::{Arc, Mutex};

use anyhow::Result;
use axum::Router;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use tower_http::trace::{DefaultMakeSpan, DefaultOnResponse, TraceLayer};
use tracing::{Level, info, warn};

use super::{registry::BoardRegistry, v0};
use crate::api_client::types::MinerState;
use crate::board::BoardRegistration;

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

/// Shared application state available to all handlers.
#[derive(Clone)]
pub(crate) struct SharedState {
    pub miner_state_rx: watch::Receiver<MinerState>,
    pub board_registry: Arc<Mutex<BoardRegistry>>,
}

/// Start the API server.
///
/// This function starts the HTTP API server and runs until the provided
/// cancellation token is triggered. It binds to localhost only by default for
/// security.
///
/// Board registrations arrive via `board_reg_rx` as boards connect. The
/// server manages the collection internally and cleans up when boards
/// disconnect.
pub async fn serve(
    config: ApiConfig,
    shutdown: CancellationToken,
    miner_state_rx: watch::Receiver<MinerState>,
    mut board_reg_rx: mpsc::Receiver<BoardRegistration>,
) -> Result<()> {
    let board_registry = Arc::new(Mutex::new(BoardRegistry::new()));

    // Drain board registrations into the registry as they arrive.
    // Exits when the sender is dropped (backplane shutdown).
    tokio::spawn({
        let registry = board_registry.clone();
        async move {
            while let Some(reg) = board_reg_rx.recv().await {
                registry.lock().unwrap_or_else(|e| e.into_inner()).push(reg);
            }
        }
    });

    let app = build_router(miner_state_rx, board_registry);

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
pub(crate) fn build_router(
    miner_state_rx: watch::Receiver<MinerState>,
    board_registry: Arc<Mutex<BoardRegistry>>,
) -> Router {
    let state = SharedState {
        miner_state_rx,
        board_registry,
    };

    Router::new()
        .nest("/api/v0", v0::routes())
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::new().level(Level::INFO))
                .on_response(DefaultOnResponse::new().level(Level::INFO)),
        )
        .with_state(state)
}