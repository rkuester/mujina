//! HTTP server lifecycle and router construction.

use std::sync::Arc;

use anyhow::Result;
use axum::{Router, response::Redirect, routing};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use tower_http::trace::{DefaultMakeSpan, DefaultOnResponse, TraceLayer};
use tracing::Level;

use crate::tracing::prelude::*;
use utoipa_axum::router::OpenApiRouter;
use utoipa_swagger_ui::SwaggerUi;

use super::{commands::SchedulerCommand, v0};
use crate::api_client::types::MinerTelemetry;
use crate::config::Configuration;
use crate::systree::Tree;

/// API server configuration.
#[derive(Debug, Clone)]
pub struct ApiConfig {
    /// Address and port to bind the API server to.
    pub bind_addr: String,
}

/// Shared application state available to all handlers.
#[derive(Clone)]
pub(crate) struct SharedState {
    pub miner_telemetry_rx: watch::Receiver<MinerTelemetry>,
    pub scheduler_cmd_tx: mpsc::Sender<SchedulerCommand>,
    pub systree: Arc<Tree>,
    pub configuration: Arc<Configuration>,
}

impl SharedState {
    /// Returns the current miner telemetry.
    pub fn miner_telemetry(&self) -> MinerTelemetry {
        self.miner_telemetry_rx.borrow().clone()
    }
}

/// Start the API server.
///
/// This function starts the HTTP API server and runs until the provided
/// cancellation token is triggered. It binds to localhost only by default for
/// security.
///
pub async fn serve(
    config: ApiConfig,
    shutdown: CancellationToken,
    miner_telemetry_rx: watch::Receiver<MinerTelemetry>,
    scheduler_cmd_tx: mpsc::Sender<SchedulerCommand>,
    systree: Arc<Tree>,
    configuration: Arc<Configuration>,
) -> Result<()> {
    let app = build_router(miner_telemetry_rx, scheduler_cmd_tx, systree, configuration);

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
    miner_telemetry_rx: watch::Receiver<MinerTelemetry>,
    scheduler_cmd_tx: mpsc::Sender<SchedulerCommand>,
    systree: Arc<Tree>,
    configuration: Arc<Configuration>,
) -> Router {
    let state = SharedState {
        miner_telemetry_rx,
        scheduler_cmd_tx,
        systree,
        configuration,
    };

    let (router, api) = OpenApiRouter::new()
        .nest("/api/v0", v0::routes())
        .with_state(state)
        .split_for_parts();

    router
        .route("/", routing::get(Redirect::permanent("/swagger-ui")))
        .route("/api", routing::get(Redirect::permanent("/swagger-ui")))
        .merge(SwaggerUi::new("/swagger-ui").url("/api/v0/openapi.json", api))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::new().level(Level::TRACE))
                .on_response(DefaultOnResponse::new().level(Level::TRACE)),
        )
}

#[cfg(test)]
mod tests {
    use http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;
    use crate::api::commands::SchedulerCommand;
    use crate::api_client::types::SourceTelemetry;
    use crate::config::{Configuration, Location};
    use crate::systree::{Cursor, Origin, Setting, within};
    use serde_json::{Value, json};

    /// Test fixtures returned by the router builder.
    struct TestFixtures {
        router: Router,
        /// A cursor on the root of the router's tree, for attaching
        /// attributes a test wants to read or write.
        systree: Cursor,
        /// Publish updated miner telemetry (e.g. after handling a command).
        _miner_tx: watch::Sender<MinerTelemetry>,
        /// Receives commands sent by PATCH handlers.
        _cmd_rx: mpsc::Receiver<SchedulerCommand>,
    }

    fn build_test_router(miner_state: MinerTelemetry) -> TestFixtures {
        let (miner_tx, miner_rx) = watch::channel(miner_state);
        let (cmd_tx, cmd_rx) = mpsc::channel::<SchedulerCommand>(16);

        let configuration = Configuration::load(Location::Defaults);
        let systree = Tree::new(configuration.clone());

        TestFixtures {
            router: build_router(miner_rx, cmd_tx, systree.clone(), configuration),
            systree: systree.root(),
            _miner_tx: miner_tx,
            _cmd_rx: cmd_rx,
        }
    }

    async fn get(app: Router, uri: &str) -> (http::StatusCode, String) {
        send(app, "GET", uri, None).await
    }

    /// Sends one request and returns its status and body.
    async fn send(
        app: Router,
        method: &str,
        uri: &str,
        body: Option<Value>,
    ) -> (http::StatusCode, String) {
        let request = Request::builder().method(method).uri(uri);
        let request = match body {
            Some(body) => request
                .header("content-type", "application/json")
                .body(axum::body::Body::from(body.to_string())),
            None => request.body(axum::body::Body::empty()),
        };
        let resp = app.oneshot(request.unwrap()).await.unwrap();
        let status = resp.status();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }

    /// Attaches a board with one reported value and one setting.
    fn attach_a_board(systree: &Cursor) -> Setting<Option<f64>> {
        let board = systree.child("boards").child("cpu-miner-0");
        board.report("model", "CPU Miner".to_string());
        let core = board.child("regulators").child("core");
        core.report("voltage_v", Some(1.15));
        core.setting_checked("target_voltage_v", None, within(1.0..=1.3))
    }

    #[tokio::test]
    async fn the_collection_of_boards_comes_from_the_tree() {
        let fixtures = build_test_router(MinerTelemetry::default());
        attach_a_board(&fixtures.systree);

        let (status, body) = get(fixtures.router.clone(), "/api/v0/boards").await;

        assert_eq!(status, 200);
        let boards: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(boards["cpu-miner-0"]["model"], "CPU Miner");
    }

    #[tokio::test]
    async fn one_board_comes_from_the_tree() {
        let fixtures = build_test_router(MinerTelemetry::default());
        attach_a_board(&fixtures.systree);

        let (status, body) = get(fixtures.router.clone(), "/api/v0/boards/cpu-miner-0").await;

        assert_eq!(status, 200);
        let board: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            board,
            json!({
                "model": "CPU Miner",
                "regulators": {"core": {"voltage_v": 1.15, "target_voltage_v": null}},
            })
        );
    }

    #[tokio::test]
    async fn a_url_names_the_subtree_it_reads() {
        let fixtures = build_test_router(MinerTelemetry::default());
        attach_a_board(&fixtures.systree);

        let (status, body) = get(
            fixtures.router.clone(),
            "/api/v0/boards/cpu-miner-0/regulators/core",
        )
        .await;

        assert_eq!(status, 200);
        assert_eq!(
            serde_json::from_str::<Value>(&body).unwrap(),
            json!({"voltage_v": 1.15, "target_voltage_v": null})
        );
    }

    #[tokio::test]
    async fn a_url_names_one_attribute_as_readily_as_a_subtree() {
        let fixtures = build_test_router(MinerTelemetry::default());
        attach_a_board(&fixtures.systree);

        let (status, body) = get(
            fixtures.router.clone(),
            "/api/v0/boards/cpu-miner-0/regulators/core/voltage_v",
        )
        .await;

        assert_eq!(status, 200);
        assert_eq!(body, "1.15");
    }

    #[tokio::test]
    async fn the_root_reads_the_whole_miner() {
        let fixtures = build_test_router(MinerTelemetry::default());
        attach_a_board(&fixtures.systree);

        let (status, body) = get(fixtures.router.clone(), "/api/v0").await;

        assert_eq!(status, 200);
        assert_eq!(
            serde_json::from_str::<Value>(&body).unwrap(),
            json!({"boards": {"cpu-miner-0": {
                "model": "CPU Miner",
                "regulators": {"core": {"voltage_v": 1.15, "target_voltage_v": null}},
            }}})
        );
    }

    #[tokio::test]
    async fn reading_a_path_with_no_attribute_is_not_found() {
        let fixtures = build_test_router(MinerTelemetry::default());

        let (status, _body) = get(
            fixtures.router.clone(),
            "/api/v0/boards/cpu-miner-9/regulators",
        )
        .await;

        assert_eq!(status, 404);
    }

    #[tokio::test]
    async fn a_write_reaches_the_setting() {
        let fixtures = build_test_router(MinerTelemetry::default());
        let target = attach_a_board(&fixtures.systree);

        let (status, _body) = send(
            fixtures.router.clone(),
            "PUT",
            "/api/v0/boards/cpu-miner-0/regulators/core/target_voltage_v",
            Some(json!(1.2)),
        )
        .await;

        assert_eq!(status, 200);
        assert_eq!(target.get(), Some(1.2));
        assert_eq!(target.origin(), Origin::Api);
    }

    #[tokio::test]
    async fn a_write_the_attribute_refuses_leaves_the_value_alone() {
        let fixtures = build_test_router(MinerTelemetry::default());
        let target = attach_a_board(&fixtures.systree);

        let (status, _body) = send(
            fixtures.router.clone(),
            "PUT",
            "/api/v0/boards/cpu-miner-0/regulators/core/target_voltage_v",
            Some(json!(2.0)),
        )
        .await;

        assert_eq!(status, 400);
        assert_eq!(target.get(), None);
    }

    #[tokio::test]
    async fn a_client_cannot_write_what_the_daemon_reports() {
        let fixtures = build_test_router(MinerTelemetry::default());
        attach_a_board(&fixtures.systree);

        let (status, _body) = send(
            fixtures.router.clone(),
            "PUT",
            "/api/v0/boards/cpu-miner-0/regulators/core/voltage_v",
            Some(json!(1.4)),
        )
        .await;

        assert_eq!(status, 400);
    }

    #[tokio::test]
    async fn a_partial_document_writes_only_what_it_names() {
        let fixtures = build_test_router(MinerTelemetry::default());
        let target = attach_a_board(&fixtures.systree);

        let (status, body) = send(
            fixtures.router.clone(),
            "PATCH",
            "/api/v0/boards/cpu-miner-0/regulators/core",
            Some(json!({"target_voltage_v": 1.25})),
        )
        .await;

        assert_eq!(status, 200);
        assert_eq!(target.get(), Some(1.25));
        assert_eq!(
            serde_json::from_str::<Value>(&body).unwrap(),
            json!({"voltage_v": 1.15, "target_voltage_v": 1.25})
        );
    }

    #[tokio::test]
    async fn a_delete_returns_a_setting_to_the_layer_beneath() {
        let fixtures = build_test_router(MinerTelemetry::default());
        let target = attach_a_board(&fixtures.systree);
        let uri = "/api/v0/boards/cpu-miner-0/regulators/core/target_voltage_v";
        send(fixtures.router.clone(), "PUT", uri, Some(json!(1.2))).await;

        let (status, _body) = send(fixtures.router.clone(), "DELETE", uri, None).await;

        assert_eq!(status, 204);
        assert_eq!(target.get(), None);
        assert_eq!(target.origin(), Origin::Builtin);
    }

    #[tokio::test]
    async fn deleting_what_was_never_written_is_not_found() {
        let fixtures = build_test_router(MinerTelemetry::default());
        attach_a_board(&fixtures.systree);

        let (status, _body) = send(
            fixtures.router.clone(),
            "DELETE",
            "/api/v0/boards/cpu-miner-0/regulators/core/target_voltage_v",
            None,
        )
        .await;

        assert_eq!(status, 404);
    }

    #[tokio::test]
    async fn a_read_names_where_each_setting_came_from_when_asked() {
        let fixtures = build_test_router(MinerTelemetry::default());
        attach_a_board(&fixtures.systree);
        let path = "/boards/cpu-miner-0/regulators/core/target_voltage_v";
        send(
            fixtures.router.clone(),
            "PUT",
            &format!("/api/v0{path}"),
            Some(json!(1.2)),
        )
        .await;

        let (status, body) = get(
            fixtures.router.clone(),
            "/api/v0/boards/cpu-miner-0/regulators/core?origins=true",
        )
        .await;

        assert_eq!(status, 200);
        assert_eq!(
            serde_json::from_str::<Value>(&body).unwrap(),
            json!({
                "values": {"voltage_v": 1.15, "target_voltage_v": 1.2},
                "origins": {path: "api"},
            })
        );
    }

    #[tokio::test]
    async fn a_standing_rule_reaches_every_matching_attribute() {
        let fixtures = build_test_router(MinerTelemetry::default());
        let target = attach_a_board(&fixtures.systree);

        let (status, _body) = send(
            fixtures.router.clone(),
            "PUT",
            "/api/v0/boards/*/regulators/core/target_voltage_v",
            Some(json!(1.1)),
        )
        .await;

        assert_eq!(status, 200);
        assert_eq!(target.get(), Some(1.1));
    }

    #[tokio::test]
    async fn health_returns_ok() {
        let fixtures = build_test_router(MinerTelemetry::default());
        let (status, body) = get(fixtures.router.clone(), "/api/v0/health").await;
        assert_eq!(status, 200);
        assert_eq!(body, "OK");
    }

    #[tokio::test]
    async fn the_miner_document_holds_the_scheduler_totals() {
        let miner_state = MinerTelemetry {
            uptime_secs: 42,
            hashrate: 1_000_000,
            shares_submitted: 5,
            sources: vec![SourceTelemetry {
                name: "pool".into(),
                url: Some("stratum+tcp://localhost:3333".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let fixtures = build_test_router(miner_state);

        let (status, body) = get(fixtures.router.clone(), "/api/v0/miner").await;
        assert_eq!(status, 200);

        let state: MinerTelemetry = serde_json::from_str(&body).unwrap();
        assert_eq!(state.uptime_secs, 42);
        assert_eq!(state.hashrate, 1_000_000);
        assert_eq!(state.shares_submitted, 5);
        assert_eq!(state.sources.len(), 1);
        assert_eq!(state.sources[0].name, "pool");
    }

    #[tokio::test]
    async fn sources_returns_list() {
        let miner_state = MinerTelemetry {
            sources: vec![
                SourceTelemetry {
                    name: "pool-a".into(),
                    url: Some("stratum+tcp://a:3333".into()),
                    ..Default::default()
                },
                SourceTelemetry {
                    name: "pool-b".into(),
                    url: None,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let fixtures = build_test_router(miner_state);

        let (status, body) = get(fixtures.router.clone(), "/api/v0/sources").await;
        assert_eq!(status, 200);

        let sources: Vec<SourceTelemetry> = serde_json::from_str(&body).unwrap();
        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0].name, "pool-a");
        assert_eq!(sources[0].url.as_deref(), Some("stratum+tcp://a:3333"));
        assert_eq!(sources[1].name, "pool-b");
        assert_eq!(sources[1].url, None);
    }

    #[tokio::test]
    async fn source_by_name_returns_match() {
        let miner_state = MinerTelemetry {
            sources: vec![SourceTelemetry {
                name: "my-pool".into(),
                url: Some("stratum+tcp://pool:3333".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let fixtures = build_test_router(miner_state);

        let (status, body) = get(fixtures.router.clone(), "/api/v0/sources/my-pool").await;
        assert_eq!(status, 200);

        let source: SourceTelemetry = serde_json::from_str(&body).unwrap();
        assert_eq!(source.name, "my-pool");
        assert_eq!(source.url.as_deref(), Some("stratum+tcp://pool:3333"));
    }

    #[tokio::test]
    async fn source_by_name_returns_404_when_missing() {
        let fixtures = build_test_router(MinerTelemetry::default());
        let (status, _body) = get(fixtures.router.clone(), "/api/v0/sources/nonexistent").await;
        assert_eq!(status, 404);
    }

    #[tokio::test]
    async fn source_difficulty_serializes_as_f64() {
        let miner_state = MinerTelemetry {
            sources: vec![SourceTelemetry {
                name: "pool".into(),
                difficulty: Some(2048.5),
                ..Default::default()
            }],
            ..Default::default()
        };
        let fixtures = build_test_router(miner_state);

        let (status, body) = get(fixtures.router.clone(), "/api/v0/sources/pool").await;
        assert_eq!(status, 200);

        let source: SourceTelemetry = serde_json::from_str(&body).unwrap();
        assert_eq!(source.difficulty, Some(2048.5));
    }

    #[tokio::test]
    async fn unknown_route_returns_404() {
        let fixtures = build_test_router(MinerTelemetry::default());
        let (status, _body) = get(fixtures.router.clone(), "/api/v0/nope").await;
        assert_eq!(status, 404);
    }
}
