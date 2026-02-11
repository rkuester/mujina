//! API v0 endpoints.
//!
//! Version 0 signals an unstable API -- breaking changes are expected
//! until the miner reaches 1.0.

use axum::{Json, Router, extract::State, routing::get};

use super::SharedState;
use crate::api_client::types::MinerState;

/// Build the v0 API routes.
pub fn routes() -> Router<SharedState> {
    Router::new()
        .route("/health", get(health))
        .route("/miner", get(get_miner))
}

/// Health check endpoint.
async fn health() -> &'static str {
    "OK"
}

/// Return the current miner state snapshot.
///
/// Combines scheduler data (hashrate, shares, sources) with board
/// snapshots collected from each board's watch channel.
async fn get_miner(State(state): State<SharedState>) -> Json<MinerState> {
    let mut miner_state = state.miner_state_rx.borrow().clone();
    miner_state.boards = state.board_registry.lock().unwrap().boards();
    Json(miner_state)
}
