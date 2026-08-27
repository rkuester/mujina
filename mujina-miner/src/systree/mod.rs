//! The systree: one tree of attributes for the whole miner.
//!
//! An attribute is one named value, and it has a path, like a file
//! in a filesystem. The hashrate of the first hash thread on a
//! Bitaxe board sits at
//! `/boards/bitaxe-gamma-e2f56f9b/threads/0/hashes_per_s`.
//!
//! A board, a job source, etc. attaches the attributes it reports
//! at startup and retains a handle to each. Reading and writing go
//! through the handle; nothing outside this module builds a path.
//! MIP-0001 specifies the tree and the API that serves it.
//!
//! Only attributes exist in memory. Nothing stands for `/boards`;
//! the path exists because the attributes under it begin with it.
//! sysfs gives such a directory a kobject, and the systree has no
//! counterpart to it.
//!
//! Two kinds of attribute live here. The daemon writes a report and
//! a client reads it. A client writes a setting and the daemon
//! obeys it, and its value comes from the configuration layers
//! through a [`Resolver`].

mod attribute;
mod cursor;
mod guard;
mod member;
mod path;
mod render;
mod resolver;
mod selector;
mod setting;
mod tree;
mod values;

#[cfg(test)]
mod tests;

pub use attribute::{AttributeKind, AttributeValue, Report, SettingValue, ValueError};
pub use cursor::Cursor;
pub use guard::{ReadGuard, WriteGuard};
pub use member::MemberStatus;
pub use path::{Path, PathError};
pub use render::leaves;
pub use resolver::{Origin, Resolver, Unconfigured};
pub use selector::Selector;
pub use setting::{Setting, within};
pub use tree::{Tree, WriteError};
