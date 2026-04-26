//! Configuration for the getblocktemplate job source.
//!
//! Built from environment variables. The chain (mainnet, signet,
//! testnet, regtest) is detected at startup via
//! `getblockchaininfo`; the address is then validated against the
//! detected chain so a tb1q signet address against a mainnet node
//! (or vice versa) fails fast with a clear error.

use std::env;
use std::path::PathBuf;
use std::str::FromStr;

use bitcoin::{Address, Network, ScriptBuf};
use serde::Deserialize;
use serde_json::json;
use thiserror::Error;

use super::rpc::{RpcAuth, RpcClient, RpcError};

const DEFAULT_VANITY: &[u8] = b"/mujina-miner/";
const DEFAULT_EXTRANONCE2_SIZE: u8 = 4;

/// Configuration for [`super::GbtSource`].
#[derive(Clone, Debug)]
pub struct GbtConfig {
    pub url: String,
    pub auth: RpcAuth,
    pub payout_script: ScriptBuf,
    /// Display form of the payout address, kept alongside
    /// `payout_script` so stats and logs don't have to re-encode it.
    pub payout_address: String,
    pub vanity_bytes: Vec<u8>,
    pub extranonce2_size: u8,
    pub network: Network,
}

/// Errors raised while building a [`GbtConfig`] from the environment.
#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("missing required env var {0}")]
    MissingEnv(&'static str),

    #[error("invalid {var}: {message}")]
    Invalid { var: &'static str, message: String },

    #[error("chain detection (getblockchaininfo) failed: {0}")]
    ChainDetect(#[from] RpcError),

    #[error("node reports unrecognized chain {0:?}")]
    UnknownChain(String),
}

/// Subset of the `getblockchaininfo` response we care about.
#[derive(Debug, Deserialize)]
struct ChainInfoResponse {
    chain: String,
}

impl GbtConfig {
    /// Build from environment variables and the URL the daemon
    /// already extracted from `MUJINA_POOL_URL`.
    ///
    /// - **Auth**: `MUJINA_POOL_USER` + `MUJINA_POOL_PASS` (the same
    ///   vars used by the Stratum source) take precedence as
    ///   HTTP-Basic credentials. Without them the source falls back
    ///   to a cookie file at `MUJINA_BITCOIND_COOKIE` if set, or the
    ///   `bitcoind` default for the network otherwise.
    /// - **Payout**: `MUJINA_PAYOUT_ADDRESS` (required); validated
    ///   against `network`.
    /// - **Coinbase message**: `MUJINA_COINBASE_MESSAGE` (optional);
    ///   defaults to `/mujina-miner/`.
    pub fn from_env(url: String, network: Network) -> Result<Self, ConfigError> {
        let auth = resolve_auth(network);
        let address_str = env::var("MUJINA_PAYOUT_ADDRESS")
            .map_err(|_| ConfigError::MissingEnv("MUJINA_PAYOUT_ADDRESS"))?;
        let payout_script = parse_payout(&address_str, network)?;
        let vanity_bytes = env::var("MUJINA_COINBASE_MESSAGE")
            .map(String::into_bytes)
            .unwrap_or_else(|_| DEFAULT_VANITY.to_vec());

        Ok(Self {
            url,
            auth,
            payout_script,
            payout_address: address_str,
            vanity_bytes,
            extranonce2_size: DEFAULT_EXTRANONCE2_SIZE,
            network,
        })
    }

    /// Build from environment variables, auto-detecting the chain
    /// by calling `getblockchaininfo` against the configured node.
    ///
    /// Auth is resolved once provisionally (mainnet cookie default
    /// if nothing else is set), used to make the chain-detect call,
    /// and then re-resolved for the detected network so the cookie
    /// path matches `bitcoind`'s actual data dir. The address is
    /// then validated against the detected chain, surfacing
    /// network-mismatch errors at startup rather than silently
    /// running with wrong-network state.
    pub async fn detect_and_build(url: String) -> Result<Self, ConfigError> {
        let address_str = env::var("MUJINA_PAYOUT_ADDRESS")
            .map_err(|_| ConfigError::MissingEnv("MUJINA_PAYOUT_ADDRESS"))?;
        let unchecked = Address::from_str(&address_str).map_err(|e| ConfigError::Invalid {
            var: "MUJINA_PAYOUT_ADDRESS",
            message: e.to_string(),
        })?;
        let vanity_bytes = env::var("MUJINA_COINBASE_MESSAGE")
            .map(String::into_bytes)
            .unwrap_or_else(|_| DEFAULT_VANITY.to_vec());

        // Provisional auth for the detection call. `resolve_auth`
        // picks user/pass when set; otherwise it falls back to the
        // mainnet default cookie path. Signet/regtest setups that
        // depend on a network-specific cookie path should set
        // `MUJINA_BITCOIND_COOKIE` explicitly.
        let provisional_auth = resolve_auth(Network::Bitcoin);
        let rpc = RpcClient::new(url.clone(), provisional_auth.clone());
        let info: ChainInfoResponse = rpc.call("getblockchaininfo", json!([])).await?;
        let network = parse_chain(&info.chain)?;

        // Re-resolve auth so the cookie default tracks the
        // detected network. Static credentials are left alone.
        let auth = match provisional_auth {
            RpcAuth::UserPass { .. } => provisional_auth,
            RpcAuth::Cookie(_) => resolve_auth(network),
        };

        let payout_script = unchecked
            .require_network(network)
            .map_err(|e| ConfigError::Invalid {
                var: "MUJINA_PAYOUT_ADDRESS",
                message: format!("address not valid for detected chain {network:?}: {e}"),
            })?
            .script_pubkey();

        Ok(Self {
            url,
            auth,
            payout_script,
            payout_address: address_str,
            vanity_bytes,
            extranonce2_size: DEFAULT_EXTRANONCE2_SIZE,
            network,
        })
    }
}

/// Map the `chain` field from `getblockchaininfo` to a
/// [`Network`]. Bitcoin Core uses `"main"` (not `"bitcoin"`) for
/// mainnet and `"test"` for testnet3; signet and regtest use the
/// names directly.
fn parse_chain(chain: &str) -> Result<Network, ConfigError> {
    match chain {
        "main" => Ok(Network::Bitcoin),
        "signet" => Ok(Network::Signet),
        "regtest" => Ok(Network::Regtest),
        "test" | "test3" | "test4" => Ok(Network::Testnet),
        other => Err(ConfigError::UnknownChain(other.to_string())),
    }
}

fn resolve_auth(network: Network) -> RpcAuth {
    if let (Ok(user), Ok(pass)) = (env::var("MUJINA_POOL_USER"), env::var("MUJINA_POOL_PASS")) {
        return RpcAuth::UserPass { user, pass };
    }
    let cookie = env::var("MUJINA_BITCOIND_COOKIE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| default_cookie_path(network));
    RpcAuth::Cookie(cookie)
}

fn default_cookie_path(network: Network) -> PathBuf {
    let home = env::var("HOME").unwrap_or_else(|_| "/".to_string());
    let subdir = match network {
        Network::Bitcoin => None,
        Network::Testnet => Some("testnet3"),
        Network::Signet => Some("signet"),
        Network::Regtest => Some("regtest"),
        _ => None,
    };
    match subdir {
        Some(s) => PathBuf::from(format!("{home}/.bitcoin/{s}/.cookie")),
        None => PathBuf::from(format!("{home}/.bitcoin/.cookie")),
    }
}

fn parse_payout(s: &str, network: Network) -> Result<ScriptBuf, ConfigError> {
    let unchecked = Address::from_str(s).map_err(|e| ConfigError::Invalid {
        var: "MUJINA_PAYOUT_ADDRESS",
        message: e.to_string(),
    })?;
    let checked = unchecked
        .require_network(network)
        .map_err(|e| ConfigError::Invalid {
            var: "MUJINA_PAYOUT_ADDRESS",
            message: e.to_string(),
        })?;
    Ok(checked.script_pubkey())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mainnet bech32 P2WPKH address from a deterministic test
    /// pubkey. Decoded once with the bitcoin crate's address
    /// machinery so we don't have to hand-encode bech32.
    fn mainnet_p2wpkh_str() -> String {
        use bitcoin::hashes::Hash;
        let script = ScriptBuf::new_p2wpkh(&bitcoin::WPubkeyHash::from_byte_array([0x42u8; 20]));
        Address::from_script(&script, Network::Bitcoin)
            .unwrap()
            .to_string()
    }

    /// Save and restore env vars so tests don't leak into each
    /// other. Tests in this module are serial-by-design via
    /// `serial_test::serial`.
    struct EnvGuard {
        saved: Vec<(&'static str, Option<String>)>,
    }

    impl EnvGuard {
        fn capture(vars: &'static [&'static str]) -> Self {
            let saved = vars.iter().map(|v| (*v, env::var(v).ok())).collect();
            for v in vars {
                unsafe { env::remove_var(v) };
            }
            Self { saved }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (k, v) in self.saved.drain(..) {
                match v {
                    Some(val) => unsafe { env::set_var(k, val) },
                    None => unsafe { env::remove_var(k) },
                }
            }
        }
    }

    const VARS: &[&str] = &[
        "MUJINA_POOL_USER",
        "MUJINA_POOL_PASS",
        "MUJINA_BITCOIND_COOKIE",
        "MUJINA_PAYOUT_ADDRESS",
        "MUJINA_COINBASE_MESSAGE",
    ];

    #[test]
    #[serial_test::serial]
    fn from_env_uses_user_pass_when_both_set() {
        let _guard = EnvGuard::capture(VARS);
        unsafe {
            env::set_var("MUJINA_POOL_USER", "alice");
            env::set_var("MUJINA_POOL_PASS", "s3cret");
            env::set_var("MUJINA_PAYOUT_ADDRESS", mainnet_p2wpkh_str());
        }
        let cfg = GbtConfig::from_env("http://node:8332".into(), Network::Bitcoin).unwrap();
        match cfg.auth {
            RpcAuth::UserPass { user, pass } => {
                assert_eq!(user, "alice");
                assert_eq!(pass, "s3cret");
            }
            other => panic!("expected UserPass, got {other:?}"),
        }
    }

    #[test]
    #[serial_test::serial]
    fn from_env_falls_back_to_cookie_path() {
        let _guard = EnvGuard::capture(VARS);
        unsafe {
            env::set_var("MUJINA_PAYOUT_ADDRESS", mainnet_p2wpkh_str());
        }
        let cfg = GbtConfig::from_env("http://node:8332".into(), Network::Bitcoin).unwrap();
        match cfg.auth {
            RpcAuth::Cookie(path) => assert!(path.ends_with(".cookie")),
            other => panic!("expected Cookie, got {other:?}"),
        }
    }

    #[test]
    #[serial_test::serial]
    fn from_env_cookie_override_wins() {
        let _guard = EnvGuard::capture(VARS);
        unsafe {
            env::set_var("MUJINA_BITCOIND_COOKIE", "/custom/path/.cookie");
            env::set_var("MUJINA_PAYOUT_ADDRESS", mainnet_p2wpkh_str());
        }
        let cfg = GbtConfig::from_env("http://node:8332".into(), Network::Bitcoin).unwrap();
        match cfg.auth {
            RpcAuth::Cookie(path) => {
                assert_eq!(path, PathBuf::from("/custom/path/.cookie"));
            }
            other => panic!("expected Cookie, got {other:?}"),
        }
    }

    #[test]
    #[serial_test::serial]
    fn from_env_requires_payout_address() {
        let _guard = EnvGuard::capture(VARS);
        let err = GbtConfig::from_env("http://node:8332".into(), Network::Bitcoin).unwrap_err();
        assert!(
            matches!(err, ConfigError::MissingEnv("MUJINA_PAYOUT_ADDRESS")),
            "got {err:?}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn from_env_rejects_address_for_wrong_network() {
        let _guard = EnvGuard::capture(VARS);
        unsafe {
            // Set a mainnet address but ask for signet.
            env::set_var("MUJINA_PAYOUT_ADDRESS", mainnet_p2wpkh_str());
        }
        let err = GbtConfig::from_env("http://node:38332".into(), Network::Signet).unwrap_err();
        assert!(
            matches!(
                err,
                ConfigError::Invalid {
                    var: "MUJINA_PAYOUT_ADDRESS",
                    ..
                }
            ),
            "got {err:?}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn from_env_uses_custom_coinbase_message() {
        let _guard = EnvGuard::capture(VARS);
        unsafe {
            env::set_var("MUJINA_PAYOUT_ADDRESS", mainnet_p2wpkh_str());
            env::set_var("MUJINA_COINBASE_MESSAGE", "/custom/");
        }
        let cfg = GbtConfig::from_env("http://node:8332".into(), Network::Bitcoin).unwrap();
        assert_eq!(cfg.vanity_bytes, b"/custom/".to_vec());
    }

    #[test]
    fn parse_chain_known_strings() {
        assert!(matches!(parse_chain("main").unwrap(), Network::Bitcoin));
        assert!(matches!(parse_chain("signet").unwrap(), Network::Signet));
        assert!(matches!(parse_chain("regtest").unwrap(), Network::Regtest));
        assert!(matches!(parse_chain("test").unwrap(), Network::Testnet));
        assert!(matches!(parse_chain("test4").unwrap(), Network::Testnet));
        assert!(matches!(
            parse_chain("nope"),
            Err(ConfigError::UnknownChain(s)) if s == "nope"
        ));
    }

    /// Spin up a tiny axum-backed JSON-RPC server that answers
    /// `getblockchaininfo` with the given chain string. Returns
    /// the URL to point a client at.
    async fn spawn_detect_server(chain: &'static str) -> String {
        use axum::{Json, Router, body::Bytes, http::StatusCode, routing::post};
        use serde_json::Value;
        use tokio::net::TcpListener;

        async fn handler(
            axum::extract::State(chain): axum::extract::State<&'static str>,
            body: Bytes,
        ) -> (StatusCode, Json<Value>) {
            let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            let method = body["method"].as_str().unwrap_or("");
            let result = match method {
                "getblockchaininfo" => json!({"chain": chain}),
                other => panic!("unexpected method: {other}"),
            };
            (
                StatusCode::OK,
                Json(json!({"result": result, "error": null, "id": "mujina"})),
            )
        }

        let app = Router::new().route("/", post(handler)).with_state(chain);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    fn signet_p2wpkh_str() -> String {
        use bitcoin::hashes::Hash;
        let script = ScriptBuf::new_p2wpkh(&bitcoin::WPubkeyHash::from_byte_array([0x42u8; 20]));
        Address::from_script(&script, Network::Signet)
            .unwrap()
            .to_string()
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn detect_and_build_sets_network_from_chain_field() {
        let _guard = EnvGuard::capture(VARS);
        unsafe {
            env::set_var("MUJINA_POOL_USER", "u");
            env::set_var("MUJINA_POOL_PASS", "p");
            env::set_var("MUJINA_PAYOUT_ADDRESS", signet_p2wpkh_str());
        }
        let url = spawn_detect_server("signet").await;
        let cfg = GbtConfig::detect_and_build(url).await.unwrap();
        assert!(matches!(cfg.network, Network::Signet));
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn detect_and_build_rejects_address_for_wrong_chain() {
        let _guard = EnvGuard::capture(VARS);
        unsafe {
            env::set_var("MUJINA_POOL_USER", "u");
            env::set_var("MUJINA_POOL_PASS", "p");
            // Mainnet address against a signet-reporting node.
            env::set_var("MUJINA_PAYOUT_ADDRESS", mainnet_p2wpkh_str());
        }
        let url = spawn_detect_server("signet").await;
        let err = GbtConfig::detect_and_build(url).await.unwrap_err();
        assert!(
            matches!(
                err,
                ConfigError::Invalid {
                    var: "MUJINA_PAYOUT_ADDRESS",
                    ..
                }
            ),
            "got {err:?}"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn detect_and_build_unknown_chain_errors_clearly() {
        let _guard = EnvGuard::capture(VARS);
        unsafe {
            env::set_var("MUJINA_POOL_USER", "u");
            env::set_var("MUJINA_POOL_PASS", "p");
            env::set_var("MUJINA_PAYOUT_ADDRESS", mainnet_p2wpkh_str());
        }
        let url = spawn_detect_server("totally-bogus").await;
        let err = GbtConfig::detect_and_build(url).await.unwrap_err();
        assert!(
            matches!(err, ConfigError::UnknownChain(ref s) if s == "totally-bogus"),
            "got {err:?}"
        );
    }
}
