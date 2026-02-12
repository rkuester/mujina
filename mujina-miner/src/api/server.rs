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
                .make_span_with(DefaultMakeSpan::new().level(Level::TRACE))
                .on_response(DefaultOnResponse::new().level(Level::TRACE)),
        )
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;
    use crate::api_client::types::{BoardState, SourceState};
    use crate::board::BoardRegistration;

    /// Build a router with pre-registered boards. Returns the router and
    /// a vec of watch senders (must be kept alive for the boards to remain
    /// connected).
    fn router_with_boards(
        miner_state: MinerState,
        board_states: Vec<BoardState>,
    ) -> (Router, Vec<watch::Sender<BoardState>>) {
        let (_miner_tx, miner_rx) = watch::channel(miner_state);

        let mut registry = BoardRegistry::new();
        let mut senders = Vec::new();
        for state in board_states {
            let (tx, rx) = watch::channel(state);
            registry.push(BoardRegistration { state_rx: rx });
            senders.push(tx);
        }

        (
            build_router(miner_rx, Arc::new(Mutex::new(registry))),
            senders,
        )
    }

    async fn get(app: Router, uri: &str) -> (http::StatusCode, String) {
        let req = Request::builder()
            .uri(uri)
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let status = resp.status();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn health_returns_ok() {
        let (app, _keep) = router_with_boards(MinerState::default(), vec![]);
        let (status, body) = get(app.clone(), "/api/v0/health").await;
        assert_eq!(status, 200);
        assert_eq!(body, "OK");
    }

    #[tokio::test]
    async fn miner_includes_boards_and_sources() {
        let miner_state = MinerState {
            uptime_secs: 42,
            hashrate: 1_000_000,
            shares_submitted: 5,
            sources: vec![SourceState {
                name: "pool".into(),
                url: Some("stratum+tcp://localhost:3333".into()),
            }],
            ..Default::default()
        };
        let board = BoardState {
            name: "test-board".into(),
            model: "TestModel".into(),
            ..Default::default()
        };
        let (app, _keep) = router_with_boards(miner_state, vec![board]);

        let (status, body) = get(app.clone(), "/api/v0/miner").await;
        assert_eq!(status, 200);

        let state: MinerState = serde_json::from_str(&body).unwrap();
        assert_eq!(state.uptime_secs, 42);
        assert_eq!(state.hashrate, 1_000_000);
        assert_eq!(state.shares_submitted, 5);
        assert_eq!(state.boards.len(), 1);
        assert_eq!(state.boards[0].name, "test-board");
        assert_eq!(state.sources.len(), 1);
        assert_eq!(state.sources[0].name, "pool");
    }

    #[tokio::test]
    async fn boards_returns_list() {
        let boards = vec![
            BoardState {
                name: "board-a".into(),
                model: "A".into(),
                ..Default::default()
            },
            BoardState {
                name: "board-b".into(),
                model: "B".into(),
                ..Default::default()
            },
        ];
        let (app, _keep) = router_with_boards(MinerState::default(), boards);

        let (status, body) = get(app.clone(), "/api/v0/boards").await;
        assert_eq!(status, 200);

        let boards: Vec<BoardState> = serde_json::from_str(&body).unwrap();
        assert_eq!(boards.len(), 2);
        assert_eq!(boards[0].name, "board-a");
        assert_eq!(boards[1].name, "board-b");
    }

    #[tokio::test]
    async fn board_by_name_returns_match() {
        let board = BoardState {
            name: "bitaxe-abc123".into(),
            model: "Bitaxe".into(),
            serial: Some("abc123".into()),
            ..Default::default()
        };
        let (app, _keep) = router_with_boards(MinerState::default(), vec![board]);

        let (status, body) = get(app.clone(), "/api/v0/boards/bitaxe-abc123").await;
        assert_eq!(status, 200);

        let board: BoardState = serde_json::from_str(&body).unwrap();
        assert_eq!(board.name, "bitaxe-abc123");
        assert_eq!(board.serial, Some("abc123".into()));
    }

    #[tokio::test]
    async fn board_by_name_returns_404_when_missing() {
        let (app, _keep) = router_with_boards(MinerState::default(), vec![]);
        let (status, _body) = get(app.clone(), "/api/v0/boards/nonexistent").await;
        assert_eq!(status, 404);
    }

    #[tokio::test]
    async fn sources_returns_list() {
        let miner_state = MinerState {
            sources: vec![
                SourceState {
                    name: "pool-a".into(),
                    url: Some("stratum+tcp://a:3333".into()),
                },
                SourceState {
                    name: "pool-b".into(),
                    url: None,
                },
            ],
            ..Default::default()
        };
        let (app, _keep) = router_with_boards(miner_state, vec![]);

        let (status, body) = get(app.clone(), "/api/v0/sources").await;
        assert_eq!(status, 200);

        let sources: Vec<SourceState> = serde_json::from_str(&body).unwrap();
        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0].name, "pool-a");
        assert_eq!(sources[0].url.as_deref(), Some("stratum+tcp://a:3333"));
        assert_eq!(sources[1].name, "pool-b");
        assert_eq!(sources[1].url, None);
    }

    #[tokio::test]
    async fn source_by_name_returns_match() {
        let miner_state = MinerState {
            sources: vec![SourceState {
                name: "my-pool".into(),
                url: Some("stratum+tcp://pool:3333".into()),
            }],
            ..Default::default()
        };
        let (app, _keep) = router_with_boards(miner_state, vec![]);

        let (status, body) = get(app.clone(), "/api/v0/sources/my-pool").await;
        assert_eq!(status, 200);

        let source: SourceState = serde_json::from_str(&body).unwrap();
        assert_eq!(source.name, "my-pool");
        assert_eq!(source.url.as_deref(), Some("stratum+tcp://pool:3333"));
    }

    #[tokio::test]
    async fn source_by_name_returns_404_when_missing() {
        let (app, _keep) = router_with_boards(MinerState::default(), vec![]);
        let (status, _body) = get(app.clone(), "/api/v0/sources/nonexistent").await;
        assert_eq!(status, 404);
    }

    #[tokio::test]
    async fn unknown_route_returns_404() {
        let (app, _keep) = router_with_boards(MinerState::default(), vec![]);
        let (status, _body) = get(app.clone(), "/api/v0/nope").await;
        assert_eq!(status, 404);
    }
}