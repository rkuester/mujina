//! Configuration for the getblocktemplate job source.
//!
//! Built from environment variables. Network detection (auto-
//! discovery from `getblockchaininfo`) lands in a later commit;
//! today the network is fixed to mainnet by the daemon.

use std::env;
use std::path::PathBuf;
use std::str::FromStr;

use bitcoin::{Address, Network, ScriptBuf};
use thiserror::Error;

use super::rpc::RpcAuth;

const DEFAULT_VANITY: &[u8] = b"/mujina-miner/";
const DEFAULT_EXTRANONCE2_SIZE: u8 = 4;

/// Configuration for [`super::GbtSource`].
#[derive(Clone, Debug)]
pub struct GbtConfig {
    pub url: String,
    pub auth: RpcAuth,
    pub payout_script: ScriptBuf,
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
            vanity_bytes,
            extranonce2_size: DEFAULT_EXTRANONCE2_SIZE,
            network,
        })
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
}
