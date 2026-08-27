//! The API over the systree.
//!
//! MIP-0001 UR4 makes a URL below the version prefix the JSON
//! Pointer into the tree, so one mechanism serves a single
//! attribute, a whole board, or the entire miner.
//!
//! The documents the earlier API serves, `/miner`, `/boards`, and
//! `/sources`, still answer at their own URLs. They are more
//! specific routes, so they shadow the tree at those three paths
//! until they retire.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path as UrlPath, Query, State},
    http::StatusCode,
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::server::SharedState;
use crate::config::Configuration;
use crate::systree::{Path, Selector, Tree, WriteError, leaves};

/// What a read asks for beyond the values.
#[derive(Debug, Default, Deserialize)]
pub struct ReadQuery {
    /// Whether to name the layer each setting's value came from.
    #[serde(default)]
    origins: bool,
}

/// Why a request could not be served.
type Refusal = (StatusCode, String);

/// Returns the subtree at a path.
///
/// With `?origins=true` the answer pairs the values with the layer
/// each setting came from, which is what MIP-0001 UR7 asks for.
pub async fn read(
    State(state): State<SharedState>,
    UrlPath(pointer): UrlPath<String>,
    Query(query): Query<ReadQuery>,
) -> Result<Json<Value>, Refusal> {
    render(&state, &parse(&pointer)?, query)
}

/// Returns the whole miner.
///
/// The root is the miner itself, and it carries no name, so it has
/// a route of its own rather than an empty wildcard.
pub async fn read_root(
    State(state): State<SharedState>,
    Query(query): Query<ReadQuery>,
) -> Result<Json<Value>, Refusal> {
    render(&state, &Path::root(), query)
}

/// Renders one subtree, with its origins when asked.
fn render(state: &SharedState, path: &Path, query: ReadQuery) -> Result<Json<Value>, Refusal> {
    let Some(values) = state.systree.render(path) else {
        return Err((StatusCode::NOT_FOUND, format!("no attribute at {path}")));
    };

    Ok(Json(if query.origins {
        json!({"values": values, "origins": state.systree.origins(path)})
    } else {
        values
    }))
}

/// Replaces the value at a path, and keeps it across restarts.
///
/// A path holding a `*` segment is a standing rule that applies to
/// every matching attribute, including attributes created tomorrow.
pub async fn replace(
    State(state): State<SharedState>,
    UrlPath(pointer): UrlPath<String>,
    Json(value): Json<Value>,
) -> Result<Json<Value>, Refusal> {
    let path = parse(&pointer)?;
    store(&state.configuration, &state.systree, &path, value)?;

    Ok(Json(state.systree.render(&path).unwrap_or(Value::Null)))
}

/// Replaces the values a partial document names, leaving the rest.
///
/// Each scalar in the document names one attribute under this path,
/// so `{"target_voltage_v": 1.2}` on a regulator writes that one
/// attribute. Each write is validated on its own; a document whose
/// first value is refused leaves the ones before it in place.
pub async fn amend(
    State(state): State<SharedState>,
    UrlPath(pointer): UrlPath<String>,
    Json(document): Json<Value>,
) -> Result<Json<Value>, Refusal> {
    let path = parse(&pointer)?;
    for (path, value) in leaves(&document, &path) {
        store(&state.configuration, &state.systree, &path, value)?;
    }

    Ok(Json(state.systree.render(&path).unwrap_or(Value::Null)))
}

/// Forgets what a client wrote at a path, uncovering the layer below.
pub async fn forget(
    State(state): State<SharedState>,
    UrlPath(pointer): UrlPath<String>,
) -> Result<StatusCode, Refusal> {
    let path = parse(&pointer)?;
    let selector = Selector::from(&path);

    if state.configuration.forget(&state.systree, &selector) {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err((
            StatusCode::NOT_FOUND,
            format!("nothing was saved at {path}"),
        ))
    }
}

/// Writes one value, reporting why the tree refused it.
fn store(
    configuration: &Arc<Configuration>,
    systree: &Arc<Tree>,
    path: &Path,
    value: Value,
) -> Result<(), Refusal> {
    configuration
        .store(systree, Selector::from(path), value)
        .map_err(|e| match e {
            WriteError::NoSuchAttribute(_) => (StatusCode::NOT_FOUND, e.to_string()),
            WriteError::Value(_) => (StatusCode::BAD_REQUEST, e.to_string()),
        })
}

/// Reads the path a URL names.
///
/// Axum hands over the URL without its leading slash, and a JSON
/// Pointer needs one. An empty URL names the root, which is the
/// miner itself, rather than a segment with an empty name.
fn parse(pointer: &str) -> Result<Path, Refusal> {
    if pointer.is_empty() {
        return Ok(Path::root());
    }

    format!("/{pointer}")
        .parse()
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("{e}")))
}
