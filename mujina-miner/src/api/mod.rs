//! HTTP API server.
//!
//! This module implements the REST API server for external control and
//! monitoring of the miner. Built on Axum, it provides endpoints for status,
//! configuration, and real-time updates.
//!
//! The API binds to localhost only by default and does not require
//! authentication for local access.

mod v0;

use std::sync::{Arc, Mutex};

use anyhow::Result;
use axum::Router;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use tower_http::trace::{DefaultMakeSpan, DefaultOnResponse, TraceLayer};
use tracing::{Level, info, warn};

use crate::api_client::types::{BoardState, MinerState};

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

/// Dynamic collection of board watch receivers.
///
/// Boards register by sending a `watch::Receiver<BoardState>` through the
/// registration channel. The registry drains new registrations and cleans
/// up disconnected boards lazily when accessed.
pub struct BoardRegistry {
    reg_rx: mpsc::Receiver<watch::Receiver<BoardState>>,
    boards: Vec<watch::Receiver<BoardState>>,
}

impl BoardRegistry {
    /// Create a new registry from a registration channel receiver.
    pub fn new(reg_rx: mpsc::Receiver<watch::Receiver<BoardState>>) -> Self {
        Self {
            reg_rx,
            boards: Vec::new(),
        }
    }

    /// Snapshot all connected boards.
    ///
    /// Drains pending registrations, removes boards whose sender has been
    /// dropped (board disconnected), and returns the current state of each.
    pub fn boards(&mut self) -> Vec<BoardState> {
        while let Ok(rx) = self.reg_rx.try_recv() {
            self.boards.push(rx);
        }
        self.boards.retain(|rx| rx.has_changed().is_ok());
        self.boards.iter().map(|rx| rx.borrow().clone()).collect()
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
/// Board watch receivers arrive via `board_reg_rx` as boards connect. The
/// server manages the collection internally and cleans up when boards
/// disconnect.
pub async fn serve(
    config: ApiConfig,
    shutdown: CancellationToken,
    miner_state_rx: watch::Receiver<MinerState>,
    board_reg_rx: mpsc::Receiver<watch::Receiver<BoardState>>,
) -> Result<()> {
    let app = build_router(miner_state_rx, board_reg_rx);

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
fn build_router(
    miner_state_rx: watch::Receiver<MinerState>,
    board_reg_rx: mpsc::Receiver<watch::Receiver<BoardState>>,
) -> Router {
    let state = SharedState {
        miner_state_rx,
        board_registry: Arc::new(Mutex::new(BoardRegistry::new(board_reg_rx))),
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
