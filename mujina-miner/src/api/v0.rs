//! API v0 endpoints.
//!
//! Version 0 signals an unstable API -- breaking changes are expected
//! until the miner reaches 1.0.

use axum::{Json, Router, extract::Path, extract::State, http::StatusCode, routing::get};

use super::SharedState;
use crate::api_client::types::{BoardState, MinerState};

/// Build the v0 API routes.
pub fn routes() -> Router<SharedState> {
    Router::new()
        .route("/health", get(health))
        .route("/miner", get(get_miner))
        .route("/boards", get(get_boards))
        .route("/boards/:name", get(get_board))
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

/// Return all connected boards.
async fn get_boards(State(state): State<SharedState>) -> Json<Vec<BoardState>> {
    Json(
        state
            .board_registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .boards(),
    )
}

/// Return a single board by name, or 404 if not found.
async fn get_board(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> Result<Json<BoardState>, StatusCode> {
    state
        .board_registry
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .boards()
        .into_iter()
        .find(|b| b.name == name)
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}
