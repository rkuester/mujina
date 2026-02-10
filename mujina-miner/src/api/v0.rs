//! API v0 endpoints.
//!
//! Version 0 signals an unstable API -- breaking changes are expected
//! until the miner reaches 1.0.

use axum::{Router, routing::get};

/// Build the v0 API routes.
pub fn routes() -> Router {
    Router::new().route("/health", get(health))
}

/// Health check endpoint.
///
/// Returns a simple OK status to verify the API is running.
async fn health() -> &'static str {
    "OK"
}
