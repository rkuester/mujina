//! Where a setting's value comes from, and who answers for it.
//!
//! The layers hold what was set and the tree holds what is in
//! effect. A setting attribute asks a resolver for its value as it
//! is created, and again whenever a layer changes, so nothing in
//! the tree waits for configuration and nothing in configuration
//! waits for the tree.

use std::fmt;

use serde_json::Value;

use super::path::Path;

/// The layer a setting's value came from.
///
/// Ordered lowest to highest, which is the precedence MIP-0001 UR3
/// fixes. The daemon writes only the saved layer.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Origin {
    /// Compiled into the daemon.
    Builtin,

    /// A file a package dropped in.
    Package,

    /// A file a system service wrote at boot.
    Runtime,

    /// A file the administrator wrote.
    Admin,

    /// What a client wrote through the API, kept across restarts.
    Saved,

    /// An environment variable, read once at startup.
    Environment,

    /// A command-line argument, read once at startup.
    Argument,

    /// A client's write, before it reaches saved config.
    Api,
}

/// Answers what a setting at a path should hold.
pub trait Resolver: Send + Sync {
    /// Returns the value in effect for a path, and the layer it came
    /// from, or `None` when no layer names the path.
    fn resolve(&self, path: &Path) -> Option<(Value, Origin)>;
}

/// A resolver with no layers.
///
/// Every setting then keeps its compiled default. Tests and the
/// early daemon use it.
pub struct Unconfigured;

impl Resolver for Unconfigured {
    fn resolve(&self, _path: &Path) -> Option<(Value, Origin)> {
        None
    }
}

impl Origin {
    /// Returns the name the API reports for the layer.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::Package => "package",
            Self::Runtime => "runtime",
            Self::Admin => "admin",
            Self::Saved => "saved",
            Self::Environment => "environment",
            Self::Argument => "argument",
            Self::Api => "api",
        }
    }
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_higher_layer_outranks_a_lower_one() {
        assert!(Origin::Saved > Origin::Admin);
        assert!(Origin::Environment > Origin::Saved);
        assert!(Origin::Builtin < Origin::Package);
    }
}
