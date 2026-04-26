//! Direct-to-node mining job source.
//!
//! Talks to a Bitcoin Core node over JSON-RPC (`getblocktemplate` /
//! `submitblock`), parallel to the Stratum v1 source. Same scheduler
//! plumbing applies; the difference is that the coinbase is built
//! locally instead of received from a pool.
//!
//! The daemon dispatches between this source and Stratum based on
//! the URL scheme of `MUJINA_POOL_URL` (`http(s)` here, `stratum*`
//! / `tcp` for Stratum).

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use bitcoin::block::Version as BlockVersion;
use serde_json::json;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use bitcoin::hash_types::TxMerkleNode;

use self::block::assemble_block;
use self::coinbase::{CoinbaseSplit, build_coinbase, compute_merkle_branches};
use self::config::GbtConfig;
use self::rpc::RpcClient;
use self::template::{GbtResponse, ParsedTemplate};
use super::{
    Extranonce2Range, GeneralPurposeBits, JobTemplate, MerkleRootKind, MerkleRootTemplate, Share,
    SourceCommand, SourceEvent, VersionTemplate,
};
use crate::tracing::prelude::*;
use crate::types::HashRate;

pub mod block;
pub mod coinbase;
pub mod config;
pub mod rpc;
pub mod stats;
pub mod template;

/// Initial delay after a fetch error before retrying.
const BACKOFF_INITIAL: Duration = Duration::from_secs(1);
/// Cap on retry delay.
const BACKOFF_MAX: Duration = Duration::from_secs(60);

/// Direct-to-node job source.
///
/// Pulls templates from a Bitcoin Core node via long-polling
/// `getblocktemplate`, builds the coinbase locally, and emits
/// [`SourceEvent::ReplaceJob`] for the scheduler. Block submission
/// (`submitblock`) is wired in a later commit.
pub struct GbtSource {
    config: GbtConfig,
    event_tx: mpsc::Sender<SourceEvent>,
    command_rx: mpsc::Receiver<SourceCommand>,
    shutdown: CancellationToken,
    rpc: RpcClient,
    expected_hashrate: HashRate,
    current: Option<TemplateState>,
    job_seq: u64,
}

/// Per-template bookkeeping kept alongside the source.
///
/// Held so the [`SubmitShare`](SourceCommand::SubmitShare) handler
/// can reconstruct the block from the share without re-fetching the
/// template.
struct TemplateState {
    template: Arc<ParsedTemplate>,
    coinbase: CoinbaseSplit,
    merkle_branches: Vec<TxMerkleNode>,
    job_id: String,
}

impl GbtSource {
    /// Create a new source.
    pub fn new(
        config: GbtConfig,
        command_rx: mpsc::Receiver<SourceCommand>,
        event_tx: mpsc::Sender<SourceEvent>,
        shutdown: CancellationToken,
    ) -> Self {
        let rpc = RpcClient::new(config.url.clone(), config.auth.clone());
        Self {
            config,
            event_tx,
            command_rx,
            shutdown,
            rpc,
            expected_hashrate: HashRate::default(),
            current: None,
            job_seq: 0,
        }
    }

    /// Human-readable name for telemetry, derived from the URL.
    pub fn name(&self) -> String {
        let trimmed = self.config.url.trim_end_matches('/');
        trimmed
            .strip_prefix("http://")
            .or_else(|| trimmed.strip_prefix("https://"))
            .unwrap_or(trimmed)
            .to_string()
    }

    /// Run the source until shutdown.
    ///
    /// Defers the first RPC call until the scheduler reports a
    /// positive hashrate (no point fetching jobs without workers).
    /// Then enters a long-poll loop: fetch a template, parse it,
    /// build the coinbase, and emit `ReplaceJob`. Errors trigger
    /// exponential backoff and a `ClearJobs` event so stale work
    /// isn't held forever.
    pub async fn run(mut self) -> Result<()> {
        info!(node = %self.config.url, "Waiting for hash threads before connecting");
        if !self.wait_for_hashrate().await {
            return Ok(());
        }

        let mut backoff = Duration::ZERO;
        let mut longpollid: Option<String> = None;

        loop {
            if !backoff.is_zero() && !self.sleep_or_handle(backoff).await {
                return Ok(());
            }

            match self.fetch_once(longpollid.clone()).await {
                FetchOutcome::Shutdown => return Ok(()),
                FetchOutcome::Got(raw) => match self.process_template(*raw).await {
                    Ok(new_lpid) => {
                        backoff = Duration::ZERO;
                        longpollid = new_lpid;
                    }
                    Err(e) => {
                        warn!(error = %e, "Failed to process template");
                        backoff = next_backoff(backoff);
                        longpollid = None;
                        let _ = self.event_tx.send(SourceEvent::ClearJobs).await;
                    }
                },
                FetchOutcome::Failed(e) => {
                    warn!(error = %e, "Template fetch failed");
                    backoff = next_backoff(backoff);
                    longpollid = None;
                    let _ = self.event_tx.send(SourceEvent::ClearJobs).await;
                }
            }
        }
    }

    /// Block until either a positive hashrate arrives or shutdown
    /// fires. Returns `false` on shutdown.
    async fn wait_for_hashrate(&mut self) -> bool {
        loop {
            tokio::select! {
                Some(cmd) = self.command_rx.recv() => match cmd {
                    SourceCommand::UpdateHashRate(rate) => {
                        self.expected_hashrate = rate;
                        if !rate.is_zero() {
                            return true;
                        }
                    }
                    SourceCommand::SubmitShare(_) => {
                        // No template yet, drop silently.
                    }
                },
                _ = self.shutdown.cancelled() => return false,
            }
        }
    }

    /// Sleep for `delay`, draining commands and aborting on shutdown.
    /// Returns `false` on shutdown.
    async fn sleep_or_handle(&mut self, delay: Duration) -> bool {
        let sleep = tokio::time::sleep(delay);
        tokio::pin!(sleep);
        loop {
            tokio::select! {
                _ = &mut sleep => return true,
                Some(cmd) = self.command_rx.recv() => self.handle_command(cmd),
                _ = self.shutdown.cancelled() => return false,
            }
        }
    }

    /// Issue one `getblocktemplate` (long-polling if `longpollid` is
    /// `Some`), interleaved with command handling and shutdown.
    async fn fetch_once(&mut self, longpollid: Option<String>) -> FetchOutcome {
        let params = if let Some(id) = longpollid {
            json!([{
                "rules": ["segwit"],
                "longpollid": id,
            }])
        } else {
            json!([{ "rules": ["segwit"] }])
        };

        // Clone the RPC handle so the in-flight future doesn't keep
        // an immutable borrow on `self` --- we want to handle
        // commands (mutable borrow) while the long-poll waits.
        let rpc = self.rpc.clone();
        let fetch = async move { rpc.call::<_, GbtResponse>("getblocktemplate", params).await };
        tokio::pin!(fetch);

        loop {
            tokio::select! {
                res = &mut fetch => return match res {
                    Ok(raw) => FetchOutcome::Got(Box::new(raw)),
                    Err(e) => FetchOutcome::Failed(anyhow::Error::from(e)),
                },
                Some(cmd) = self.command_rx.recv() => self.handle_command(cmd),
                _ = self.shutdown.cancelled() => return FetchOutcome::Shutdown,
            }
        }
    }

    /// Parse a freshly-fetched template, build the coinbase + job,
    /// emit `ReplaceJob`, and return the new `longpollid`.
    async fn process_template(&mut self, raw: GbtResponse) -> Result<Option<String>> {
        let parsed = Arc::new(raw.parse()?);
        let coinbase = build_coinbase(
            &parsed,
            &self.config.payout_script,
            &self.config.vanity_bytes,
            self.config.extranonce2_size,
        )?;

        let non_coinbase_txids: Vec<_> = parsed.transactions.iter().map(|t| t.txid).collect();
        let merkle_branches = compute_merkle_branches(&non_coinbase_txids);

        self.job_seq = self.job_seq.wrapping_add(1);
        let job_id = format!("gbt-{}-{}", parsed.height, self.job_seq);

        let job = JobTemplate {
            id: job_id.clone(),
            prev_blockhash: parsed.prev_blockhash,
            version: VersionTemplate::new(
                BlockVersion::from_consensus(parsed.version),
                GeneralPurposeBits::none(),
            )?,
            bits: parsed.bits,
            share_target: parsed.target,
            time: parsed.curtime,
            merkle_root: MerkleRootKind::Computed(MerkleRootTemplate {
                coinbase1: coinbase.coinbase1.clone(),
                extranonce1: coinbase.extranonce1.clone(),
                extranonce2_range: Extranonce2Range::new(self.config.extranonce2_size)?,
                coinbase2: coinbase.coinbase2.clone(),
                merkle_branches: merkle_branches.clone(),
            }),
        };

        let new_lpid = parsed.longpollid.clone();
        self.current = Some(TemplateState {
            template: parsed,
            coinbase,
            merkle_branches,
            job_id,
        });

        self.event_tx.send(SourceEvent::ReplaceJob(job)).await?;
        Ok(new_lpid)
    }

    /// Handle a command that arrives during the long-poll wait or
    /// the inter-fetch sleep. `SubmitShare` spawns the actual
    /// `submitblock` call as a separate task so the source's main
    /// loop keeps responding to events.
    fn handle_command(&mut self, cmd: SourceCommand) {
        match cmd {
            SourceCommand::UpdateHashRate(rate) => self.expected_hashrate = rate,
            SourceCommand::SubmitShare(share) => self.spawn_submit(share),
        }
    }

    fn spawn_submit(&self, share: Share) {
        let Some(state) = self.current.as_ref() else {
            warn!(
                job_id = %share.job_id,
                "Share arrived without an active template; dropping"
            );
            return;
        };
        if state.job_id != share.job_id {
            warn!(
                share_job = %share.job_id,
                current_job = %state.job_id,
                "Share for stale job; dropping"
            );
            return;
        }

        let block_hex = match assemble_block(
            &state.template,
            &state.coinbase,
            &state.merkle_branches,
            &share,
        ) {
            Ok(hex) => hex,
            Err(e) => {
                error!(
                    job_id = %share.job_id,
                    error = %e,
                    "Failed to assemble block"
                );
                return;
            }
        };

        let rpc = self.rpc.clone();
        let job_id = share.job_id.clone();
        info!(
            job_id = %job_id,
            nonce = format!("{:#x}", share.nonce),
            "Submitting block to node"
        );
        tokio::spawn(async move {
            match rpc.submit_block(&block_hex).await {
                Ok(None) => info!(job_id = %job_id, "Block accepted by node"),
                Ok(Some(reason)) => warn!(
                    job_id = %job_id,
                    reason = %reason,
                    "Block rejected by node"
                ),
                Err(e) => error!(
                    job_id = %job_id,
                    error = %e,
                    "submitblock RPC failed"
                ),
            }
        });
    }
}

/// Outcome of a single `getblocktemplate` call.
enum FetchOutcome {
    Got(Box<GbtResponse>),
    Failed(anyhow::Error),
    Shutdown,
}

fn next_backoff(current: Duration) -> Duration {
    if current.is_zero() {
        BACKOFF_INITIAL
    } else {
        (current * 2).min(BACKOFF_MAX)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::Router;
    use axum::extract::State;
    use axum::http::StatusCode;
    use axum::routing::post;
    use bitcoin::hashes::Hash;
    use bitcoin::{Network, ScriptBuf, WPubkeyHash};
    use parking_lot::Mutex;
    use serde_json::Value;
    use tokio::net::TcpListener;

    use super::rpc::RpcAuth;
    use super::*;

    fn payout_script() -> ScriptBuf {
        ScriptBuf::new_p2wpkh(&WPubkeyHash::from_byte_array([0x42u8; 20]))
    }

    fn test_config(url: String) -> GbtConfig {
        GbtConfig {
            url,
            auth: RpcAuth::UserPass {
                user: "u".into(),
                pass: "p".into(),
            },
            payout_script: payout_script(),
            vanity_bytes: b"/test/".to_vec(),
            extranonce2_size: 4,
            network: Network::Bitcoin,
        }
    }

    fn template_response(height: u32, longpollid: &str) -> Value {
        json!({
            "version": 0x20000000_u32,
            "previousblockhash": "00000000000000000001543bc4d2b5e1434cb64af8995e21fe15ca708a6ac9e3",
            "transactions": [],
            "coinbasevalue": 312500000_u64,
            "longpollid": longpollid,
            "target": "0000000000000000000302a90000000000000000000000000000000000000000",
            "mintime": 1738182000,
            "curtime": 1738182030,
            "bits": "17029a8a",
            "height": height,
            "default_witness_commitment": "6a24aa21a9edb395560ab72068c2afb1d7e6c7db26b8134820\
                89d2c4d2471b9036c5e826140601",
        })
    }

    #[derive(Default, Clone)]
    struct ServerBehavior {
        responses: Vec<Value>,
        request_count: usize,
        hang_after: Option<usize>,
    }

    async fn spawn_server(behavior: Arc<Mutex<ServerBehavior>>) -> String {
        let app = Router::new().route("/", post(handler)).with_state(behavior);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    async fn handler(
        State(state): State<Arc<Mutex<ServerBehavior>>>,
        body: axum::body::Bytes,
    ) -> (StatusCode, axum::Json<Value>) {
        let _ = body;
        let n = {
            let mut b = state.lock();
            let n = b.request_count;
            b.request_count = b.request_count.saturating_add(1);
            n
        };

        // If asked, hang requests at or after `hang_after`.
        let hang = {
            let b = state.lock();
            b.hang_after.map(|h| n >= h).unwrap_or(false)
        };
        if hang {
            std::future::pending::<()>().await;
        }

        let response = {
            let b = state.lock();
            b.responses
                .get(n)
                .cloned()
                .unwrap_or_else(|| b.responses.last().cloned().unwrap_or(Value::Null))
        };
        (
            StatusCode::OK,
            axum::Json(json!({"result": response, "error": null, "id": "mujina"})),
        )
    }

    #[tokio::test]
    async fn first_template_emits_replace_job() {
        let behavior = Arc::new(Mutex::new(ServerBehavior {
            responses: vec![template_response(881_423, "lpid-1")],
            request_count: 0,
            hang_after: Some(1),
        }));
        let url = spawn_server(behavior.clone()).await;

        let (event_tx, mut event_rx) = mpsc::channel(8);
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let shutdown = CancellationToken::new();

        let source = GbtSource::new(test_config(url), cmd_rx, event_tx, shutdown.clone());
        let join = tokio::spawn(async move { source.run().await });

        // Kick the source out of phase 1.
        cmd_tx
            .send(SourceCommand::UpdateHashRate(HashRate::from_terahashes(
                1.0,
            )))
            .await
            .unwrap();

        let event = tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
            .await
            .expect("source emitted no event before timeout")
            .expect("channel closed");
        match event {
            SourceEvent::ReplaceJob(job) => {
                assert!(job.id.starts_with("gbt-881423-"));
                assert_eq!(job.time, 1738182030);
            }
            other => panic!("expected ReplaceJob, got {other:?}"),
        }

        shutdown.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(2), join).await;
    }

    #[tokio::test]
    async fn shutdown_during_long_poll_returns_quickly() {
        let behavior = Arc::new(Mutex::new(ServerBehavior {
            responses: vec![template_response(881_423, "lpid-1")],
            request_count: 0,
            // Hang on the second request (the long-poll after the
            // first template lands).
            hang_after: Some(1),
        }));
        let url = spawn_server(behavior.clone()).await;

        let (event_tx, mut event_rx) = mpsc::channel(8);
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let shutdown = CancellationToken::new();

        let source = GbtSource::new(test_config(url), cmd_rx, event_tx, shutdown.clone());
        let join = tokio::spawn(async move { source.run().await });

        cmd_tx
            .send(SourceCommand::UpdateHashRate(HashRate::from_terahashes(
                1.0,
            )))
            .await
            .unwrap();

        // Wait for the first event so we know the source is now in
        // its long-poll for the second request.
        let _ = tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
            .await
            .expect("first event should arrive")
            .expect("channel closed");

        // Now cancel; run() must return promptly.
        let start = tokio::time::Instant::now();
        shutdown.cancel();
        let result = tokio::time::timeout(Duration::from_secs(2), join)
            .await
            .expect("source did not shut down in time");
        assert!(result.is_ok(), "join error: {result:?}");
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "shutdown took too long: {:?}",
            start.elapsed()
        );
    }

    #[tokio::test]
    async fn second_long_poll_uses_returned_longpollid() {
        let behavior = Arc::new(Mutex::new(ServerBehavior {
            responses: vec![
                template_response(100, "lpid-A"),
                template_response(101, "lpid-B"),
            ],
            request_count: 0,
            hang_after: Some(2),
        }));
        let captured_bodies: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));

        // Re-implement the handler to capture bodies.
        let app_state = (behavior.clone(), captured_bodies.clone());
        let app = Router::new()
            .route(
                "/",
                post(
                    |State(state): State<(Arc<Mutex<ServerBehavior>>, Arc<Mutex<Vec<Value>>>)>,
                     body: axum::body::Bytes| async move {
                        let body_value: Value =
                            serde_json::from_slice(&body).unwrap_or(Value::Null);
                        state.1.lock().push(body_value);

                        let n = {
                            let mut b = state.0.lock();
                            let n = b.request_count;
                            b.request_count += 1;
                            n
                        };
                        let hang = {
                            let b = state.0.lock();
                            b.hang_after.map(|h| n >= h).unwrap_or(false)
                        };
                        if hang {
                            std::future::pending::<()>().await;
                        }
                        let response = {
                            let b = state.0.lock();
                            b.responses.get(n).cloned().unwrap_or_else(|| Value::Null)
                        };
                        (
                            StatusCode::OK,
                            axum::Json(json!({"result": response, "error": null, "id": "mujina"})),
                        )
                    },
                ),
            )
            .with_state(app_state);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let url = format!("http://{addr}");

        let (event_tx, mut event_rx) = mpsc::channel(8);
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let shutdown = CancellationToken::new();

        let source = GbtSource::new(test_config(url), cmd_rx, event_tx, shutdown.clone());
        let join = tokio::spawn(async move { source.run().await });

        cmd_tx
            .send(SourceCommand::UpdateHashRate(HashRate::from_terahashes(
                1.0,
            )))
            .await
            .unwrap();

        // Receive both templates, then shut down.
        let _ = tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
            .await
            .unwrap()
            .unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
            .await
            .unwrap()
            .unwrap();

        shutdown.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(2), join).await;

        let bodies = captured_bodies.lock().clone();
        assert!(bodies.len() >= 2);
        // First call: no longpollid.
        let p1 = &bodies[0]["params"][0];
        assert!(p1.get("longpollid").is_none() || p1["longpollid"].is_null());
        // Second call: longpollid = "lpid-A" (from the first
        // template's response).
        let p2 = &bodies[1]["params"][0];
        assert_eq!(p2["longpollid"], json!("lpid-A"));
    }

    #[test]
    fn name_strips_url_scheme() {
        let cfg = test_config("http://node.example:8332".into());
        let (_etx, _erx) = mpsc::channel(1);
        let (_ctx, crx) = mpsc::channel(1);
        let source = GbtSource::new(cfg, crx, _etx, CancellationToken::new());
        assert_eq!(source.name(), "node.example:8332");
    }

    #[test]
    fn next_backoff_doubles_then_caps() {
        assert_eq!(next_backoff(Duration::ZERO), BACKOFF_INITIAL);
        let mut d = BACKOFF_INITIAL;
        for _ in 0..20 {
            d = next_backoff(d);
        }
        assert!(d <= BACKOFF_MAX);
    }
}
