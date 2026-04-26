//! JSON-RPC client for talking to a Bitcoin Core node.
//!
//! Provides the transport, auth (cookie-file or static user/pass), and
//! a generic [`RpcClient::call`] method. Specific API wrappers
//! (`getblocktemplate`, `getblock`) live in sibling submodules and are
//! added in later commits. [`RpcClient::submit_block`] is included
//! here because its non-standard return shape (`null` on success, a
//! reject-reason string on failure, no JSON-RPC error envelope)
//! warrants special handling.

use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::RwLock;
use reqwest::{Client, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use thiserror::Error;
use tokio::fs;

/// Errors from the JSON-RPC client.
#[derive(Error, Debug)]
pub enum RpcError {
    /// HTTP transport failure (connection refused, timeout, etc.).
    #[error("transport: {0}")]
    Transport(#[from] reqwest::Error),

    /// Failed to read the cookie file.
    #[error("cookie read at {path}: {source}")]
    CookieRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// Cookie file did not contain a `user:pass` pair.
    #[error("malformed cookie at {path}")]
    MalformedCookie { path: PathBuf },

    /// HTTP authentication rejected (401) even after refreshing.
    #[error("authentication failed")]
    Unauthorized,

    /// JSON-RPC error envelope returned by the node.
    #[error("rpc {code}: {message}")]
    Rpc { code: i64, message: String },

    /// Response body could not be decoded as the expected type.
    #[error("decode: {0}")]
    Decode(#[from] serde_json::Error),

    /// HTTP status not handled by any specific case above.
    #[error("http {0}")]
    Http(StatusCode),
}

/// How to authenticate with the node.
#[derive(Clone, Debug)]
pub enum RpcAuth {
    /// Read `user:pass` from the named cookie file. Bitcoin Core
    /// rotates this file on restart, so the client re-reads it on
    /// authentication failure.
    Cookie(PathBuf),
    /// Static credentials.
    UserPass { user: String, pass: String },
}

/// JSON-RPC client for `bitcoind`.
///
/// Cheap to clone: the inner state is shared via `Arc`, so multiple
/// callers share a single connection pool and credential cache.
#[derive(Clone)]
pub struct RpcClient {
    inner: Arc<RpcInner>,
}

struct RpcInner {
    http: Client,
    url: String,
    auth: RpcAuth,
    cached: RwLock<Option<(String, String)>>,
}

impl RpcClient {
    /// Construct a new client.
    ///
    /// `url` is the node's RPC endpoint, e.g. `http://127.0.0.1:8332`.
    pub fn new(url: String, auth: RpcAuth) -> Self {
        Self {
            inner: Arc::new(RpcInner {
                http: Client::new(),
                url,
                auth,
                cached: RwLock::new(None),
            }),
        }
    }

    /// Make a JSON-RPC call. Retries once on HTTP 401 to handle cookie
    /// rotation across `bitcoind` restarts.
    pub async fn call<P, R>(&self, method: &str, params: P) -> Result<R, RpcError>
    where
        P: Serialize,
        R: DeserializeOwned,
    {
        let body = json!({
            "jsonrpc": "1.0",
            "id": "mujina",
            "method": method,
            "params": params,
        });

        for force_refresh in [false, true] {
            let (user, pass) = self.resolve_auth(force_refresh).await?;
            let resp = self
                .inner
                .http
                .post(&self.inner.url)
                .basic_auth(user, Some(pass))
                .json(&body)
                .send()
                .await?;

            if resp.status() == StatusCode::UNAUTHORIZED {
                if force_refresh {
                    return Err(RpcError::Unauthorized);
                }
                continue;
            }
            return parse_response::<R>(resp).await;
        }

        unreachable!("loop returns on the second iteration")
    }

    /// Submit a block. Returns `Ok(None)` on accept, `Ok(Some(reason))`
    /// on local-node rejection.
    ///
    /// `submitblock` returns `null` on success and a single reject-
    /// reason string on failure (e.g. `"bad-signet-blksig"` on signet
    /// without signing rights). There is no JSON-RPC error envelope
    /// for the rejection case --- the rejection arrives as the
    /// `result` field, not the `error` field.
    pub async fn submit_block(&self, block_hex: &str) -> Result<Option<String>, RpcError> {
        let result: Option<String> = self.call("submitblock", json!([block_hex])).await?;
        Ok(result)
    }

    /// Resolve auth to (user, pass), reading the cookie file if needed.
    async fn resolve_auth(&self, force_refresh: bool) -> Result<(String, String), RpcError> {
        if !force_refresh && let Some(cached) = self.inner.cached.read().clone() {
            return Ok(cached);
        }

        let resolved = match &self.inner.auth {
            RpcAuth::UserPass { user, pass } => (user.clone(), pass.clone()),
            RpcAuth::Cookie(path) => {
                let contents =
                    fs::read_to_string(path)
                        .await
                        .map_err(|source| RpcError::CookieRead {
                            path: path.clone(),
                            source,
                        })?;
                let trimmed = contents.trim();
                let (user, pass) = trimmed
                    .split_once(':')
                    .ok_or_else(|| RpcError::MalformedCookie { path: path.clone() })?;
                (user.to_string(), pass.to_string())
            }
        };

        *self.inner.cached.write() = Some(resolved.clone());
        Ok(resolved)
    }
}

async fn parse_response<R: DeserializeOwned>(resp: reqwest::Response) -> Result<R, RpcError> {
    let status = resp.status();
    let bytes = resp.bytes().await?;

    // Bitcoin Core returns 500 with a JSON-RPC error envelope for
    // method-level failures, and 200 with `error: null` for success.
    // Try to parse as an envelope first regardless of status.
    if let Ok(envelope) = serde_json::from_slice::<RpcEnvelope<R>>(&bytes) {
        if let Some(err) = envelope.error {
            return Err(RpcError::Rpc {
                code: err.code,
                message: err.message,
            });
        }
        // Successful envelopes always have a `result` field, even if
        // its value is `null`. Deserialize the field directly so that
        // `Option<T>` results work correctly.
        return Ok(serde_json::from_value(envelope.result)?);
    }

    if !status.is_success() {
        return Err(RpcError::Http(status));
    }

    Err(RpcError::Decode(
        serde_json::from_slice::<Value>(&bytes).unwrap_err(),
    ))
}

#[derive(serde::Deserialize)]
struct RpcEnvelope<R> {
    #[serde(default = "Value::default")]
    result: Value,
    error: Option<RpcRpcError>,
    #[serde(default)]
    #[allow(dead_code)]
    id: Option<Value>,
    #[serde(skip)]
    _marker: std::marker::PhantomData<R>,
}

#[derive(serde::Deserialize)]
struct RpcRpcError {
    code: i64,
    message: String,
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::Router;
    use axum::body::Bytes;
    use axum::extract::State;
    use axum::http::HeaderMap;
    use axum::response::IntoResponse;
    use axum::routing::post;
    use parking_lot::Mutex;
    use serde_json::json;
    use tokio::net::TcpListener;
    use tokio::task::JoinHandle;
    use tokio_util::sync::CancellationToken;

    use super::*;

    /// Captures one request and returns a configurable response.
    struct MockServer {
        url: String,
        captured: Arc<Mutex<Vec<Captured>>>,
        shutdown: CancellationToken,
        join: Option<JoinHandle<()>>,
    }

    #[derive(Debug, Clone)]
    struct Captured {
        auth_header: Option<String>,
        body: Value,
    }

    #[derive(Clone)]
    struct MockState {
        captured: Arc<Mutex<Vec<Captured>>>,
        responder: Arc<dyn Fn(usize) -> (StatusCode, Value) + Send + Sync>,
    }

    impl MockServer {
        async fn start<F>(responder: F) -> Self
        where
            F: Fn(usize) -> (StatusCode, Value) + Send + Sync + 'static,
        {
            let captured = Arc::new(Mutex::new(Vec::new()));
            let state = MockState {
                captured: captured.clone(),
                responder: Arc::new(responder),
            };
            let app = Router::new().route("/", post(handler)).with_state(state);

            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let url = format!("http://{addr}");
            let shutdown = CancellationToken::new();
            let shutdown_inner = shutdown.clone();
            let join = tokio::spawn(async move {
                axum::serve(listener, app)
                    .with_graceful_shutdown(async move { shutdown_inner.cancelled().await })
                    .await
                    .unwrap();
            });

            MockServer {
                url,
                captured,
                shutdown,
                join: Some(join),
            }
        }

        fn captured(&self) -> Vec<Captured> {
            self.captured.lock().clone()
        }
    }

    impl Drop for MockServer {
        fn drop(&mut self) {
            self.shutdown.cancel();
            if let Some(join) = self.join.take() {
                join.abort();
            }
        }
    }

    async fn handler(
        State(state): State<MockState>,
        headers: HeaderMap,
        body: Bytes,
    ) -> impl IntoResponse {
        let auth_header = headers
            .get("authorization")
            .and_then(|h| h.to_str().ok())
            .map(|s| s.to_string());
        let body_value: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);

        let mut guard = state.captured.lock();
        let n = guard.len();
        guard.push(Captured {
            auth_header,
            body: body_value,
        });
        drop(guard);

        let (status, body) = (state.responder)(n);
        (status, axum::Json(body))
    }

    /// Convenience: a responder that always returns the given JSON
    /// envelope with HTTP 200.
    fn ok(result: Value) -> impl Fn(usize) -> (StatusCode, Value) + Send + Sync + 'static {
        move |_| {
            (
                StatusCode::OK,
                json!({"result": result.clone(), "error": null, "id": "mujina"}),
            )
        }
    }

    #[tokio::test]
    async fn user_pass_auth_sends_basic_header() {
        let server = MockServer::start(ok(json!(123))).await;
        let client = RpcClient::new(
            server.url.clone(),
            RpcAuth::UserPass {
                user: "alice".into(),
                pass: "s3cret".into(),
            },
        );

        let _: i64 = client.call("getblockcount", json!([])).await.unwrap();

        let cap = server.captured();
        assert_eq!(cap.len(), 1);
        // base64("alice:s3cret") == "YWxpY2U6czNjcmV0"
        assert_eq!(
            cap[0].auth_header.as_deref(),
            Some("Basic YWxpY2U6czNjcmV0")
        );
    }

    #[tokio::test]
    async fn cookie_auth_reads_file_and_sends_basic_header() {
        let dir = tempdir();
        let cookie_path = dir.join(".cookie");
        std::fs::write(&cookie_path, "__cookie__:abc123").unwrap();

        let server = MockServer::start(ok(json!(0))).await;
        let client = RpcClient::new(server.url.clone(), RpcAuth::Cookie(cookie_path.clone()));

        let _: i64 = client.call("getblockcount", json!([])).await.unwrap();

        let cap = server.captured();
        // base64("__cookie__:abc123") == "X19jb29raWVfXzphYmMxMjM="
        assert_eq!(
            cap[0].auth_header.as_deref(),
            Some("Basic X19jb29raWVfXzphYmMxMjM=")
        );
    }

    #[tokio::test]
    async fn unauthorized_triggers_cookie_reread_then_succeeds() {
        let dir = tempdir();
        let cookie_path = dir.join(".cookie");
        std::fs::write(&cookie_path, "__cookie__:old").unwrap();

        // First call: respond 401. Test rewrites the cookie file
        // before the retry. Second call: respond 200.
        let cookie_for_responder = cookie_path.clone();
        let server = MockServer::start(move |n| match n {
            0 => {
                std::fs::write(&cookie_for_responder, "__cookie__:new").unwrap();
                (StatusCode::UNAUTHORIZED, Value::Null)
            }
            _ => (
                StatusCode::OK,
                json!({"result": 42, "error": null, "id": "mujina"}),
            ),
        })
        .await;

        let client = RpcClient::new(server.url.clone(), RpcAuth::Cookie(cookie_path.clone()));
        let result: i64 = client.call("getblockcount", json!([])).await.unwrap();
        assert_eq!(result, 42);

        let cap = server.captured();
        assert_eq!(cap.len(), 2);
        // base64("__cookie__:old") == "X19jb29raWVfXzpvbGQ="
        assert_eq!(
            cap[0].auth_header.as_deref(),
            Some("Basic X19jb29raWVfXzpvbGQ=")
        );
        // base64("__cookie__:new") == "X19jb29raWVfXzpuZXc="
        assert_eq!(
            cap[1].auth_header.as_deref(),
            Some("Basic X19jb29raWVfXzpuZXc=")
        );
    }

    #[tokio::test]
    async fn unauthorized_after_retry_returns_unauthorized_error() {
        let server = MockServer::start(|_| (StatusCode::UNAUTHORIZED, Value::Null)).await;
        let client = RpcClient::new(
            server.url.clone(),
            RpcAuth::UserPass {
                user: "u".into(),
                pass: "p".into(),
            },
        );

        let err = client
            .call::<_, i64>("getblockcount", json!([]))
            .await
            .unwrap_err();
        assert!(matches!(err, RpcError::Unauthorized), "got {err:?}");
    }

    #[tokio::test]
    async fn rpc_error_envelope_is_returned_as_typed_error() {
        let server = MockServer::start(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({
                    "result": null,
                    "error": {"code": -8, "message": "Block not found"},
                    "id": "mujina",
                }),
            )
        })
        .await;
        let client = RpcClient::new(
            server.url.clone(),
            RpcAuth::UserPass {
                user: "u".into(),
                pass: "p".into(),
            },
        );

        let err = client
            .call::<_, Value>("getblock", json!([]))
            .await
            .unwrap_err();
        match err {
            RpcError::Rpc { code, message } => {
                assert_eq!(code, -8);
                assert_eq!(message, "Block not found");
            }
            other => panic!("expected RpcError::Rpc, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn submit_block_success_returns_none() {
        let server = MockServer::start(ok(Value::Null)).await;
        let client = RpcClient::new(
            server.url.clone(),
            RpcAuth::UserPass {
                user: "u".into(),
                pass: "p".into(),
            },
        );

        let result = client.submit_block("deadbeef").await.unwrap();
        assert_eq!(result, None);

        let cap = server.captured();
        assert_eq!(cap[0].body["method"], "submitblock");
        assert_eq!(cap[0].body["params"], json!(["deadbeef"]));
    }

    #[tokio::test]
    async fn submit_block_rejection_returns_reason_string() {
        let server = MockServer::start(ok(json!("bad-signet-blksig"))).await;
        let client = RpcClient::new(
            server.url.clone(),
            RpcAuth::UserPass {
                user: "u".into(),
                pass: "p".into(),
            },
        );

        let result = client.submit_block("deadbeef").await.unwrap();
        assert_eq!(result, Some("bad-signet-blksig".to_string()));
    }

    #[tokio::test]
    async fn malformed_cookie_returns_typed_error() {
        let dir = tempdir();
        let cookie_path = dir.join(".cookie");
        std::fs::write(&cookie_path, "no-colon-here").unwrap();

        let client = RpcClient::new(
            "http://127.0.0.1:1".into(),
            RpcAuth::Cookie(cookie_path.clone()),
        );

        let err = client
            .call::<_, Value>("getblockcount", json!([]))
            .await
            .unwrap_err();
        match err {
            RpcError::MalformedCookie { path } => assert_eq!(path, cookie_path),
            other => panic!("expected MalformedCookie, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn missing_cookie_returns_typed_error() {
        let client = RpcClient::new(
            "http://127.0.0.1:1".into(),
            RpcAuth::Cookie(PathBuf::from("/definitely/not/a/real/cookie/path")),
        );

        let err = client
            .call::<_, Value>("getblockcount", json!([]))
            .await
            .unwrap_err();
        assert!(matches!(err, RpcError::CookieRead { .. }), "got {err:?}");
    }

    /// Make a fresh tempdir-style path for cookie tests. We avoid a
    /// dev-dependency on `tempfile` by using the OS temp dir plus a
    /// unique suffix; tests clean up with `Drop`-like RAII via
    /// `_TempDir`.
    fn tempdir() -> PathBuf {
        let base = std::env::temp_dir();
        let unique = format!(
            "mujina-rpc-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let path = base.join(unique);
        std::fs::create_dir_all(&path).unwrap();
        path
    }
}
