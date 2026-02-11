//! API v0 endpoints.
//!
//! Version 0 signals an unstable API -- breaking changes are expected
//! until the miner reaches 1.0.

use axum::{Json, Router, extract::State, routing::get};
use tokio::sync::watch;

use crate::api_client::types::MinerState;

/// Shared application state available to all handlers.
type AppState = watch::Receiver<MinerState>;

/// Build the v0 API routes.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/health", get(health))
        .route("/miner", get(get_miner))
}

/// Health check endpoint.
async fn health() -> &'static str {
    "OK"
}

/// Return the current miner state snapshot.
async fn get_miner(State(rx): State<AppState>) -> Json<MinerState> {
    Json(rx.borrow().clone())
}
