//! Direct-to-node mining job source.
//!
//! Talks to a Bitcoin Core node over JSON-RPC (`getblocktemplate` /
//! `submitblock`), parallel to the Stratum v1 source. The full source
//! struct (`GbtSource`) and run loop are added in a later commit; this
//! file currently only wires up submodules so they compile.

pub mod coinbase;
pub mod rpc;
pub mod template;
