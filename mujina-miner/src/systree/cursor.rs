//! Cursors: a path into the tree and the ability to create attributes under it.

use std::sync::Arc;

use serde_json::Value;
use tokio::sync::watch;

use super::attribute::{Attribute, AttributeValue, Report};
use super::guard::{ReadGuard, WriteGuard};
use super::path::Path;
use super::render::render_json;
use super::tree::Tree;

/// A path into the tree and the tree it names.
///
/// A component receives a cursor, creates its values under it, and
/// retains the handles. Cloning copies the path and increments a
/// reference count, so passing cursors is cheap.
#[derive(Clone)]
pub struct Cursor {
    tree: Arc<Tree>,
    path: Path,
}

impl Cursor {
    /// Returns the path this cursor names.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns a cursor on the named child.
    pub fn child(&self, name: &str) -> Self {
        Self {
            tree: Arc::clone(&self.tree),
            path: self.path.child(name),
        }
    }

    /// Attaches an attribute for the caller to report and returns its
    /// handle.
    ///
    /// The unit joins the attribute's name, so `voltage` becomes
    /// `voltage_v`. Pass the name without the unit suffix and the
    /// two stay in sync.
    pub fn report<T: AttributeValue>(&self, name: &str, initial: T) -> Report<T> {
        let (sender, _) = watch::channel(initial);
        let sender = Arc::new(sender);

        let attribute: Arc<dyn Attribute> = sender.clone();
        self.tree
            .attach(self.path.child(&with_unit(name, T::UNIT)), attribute);

        Report::new(sender)
    }

    /// Returns the subtree under this cursor as JSON.
    ///
    /// Returns `None` when no attribute lies at or under this path,
    /// keeping a missing node distinct from an attribute holding null.
    ///
    /// Holds the tree's read lock, so a render never returns a
    /// torn document.
    pub fn render_json(&self) -> Option<Value> {
        render_json(self.read().attributes(), &self.path)
    }

    /// Opens the tree for reading.
    ///
    /// Values read through the guard come from the same instant.
    /// Reading a single value needs no guard. Release the guard
    /// before attaching an attribute or opening a write guard.
    pub fn read(&self) -> ReadGuard<'_> {
        ReadGuard::open(&self.tree)
    }

    /// Opens the tree for writing.
    ///
    /// The guard covers the whole tree, not the subtree under this
    /// cursor. Hold it only while writing; attaching an attribute while
    /// holding a guard deadlocks.
    pub fn write(&self) -> WriteGuard<'_> {
        WriteGuard::open(&self.tree)
    }

    pub(super) fn new(tree: Arc<Tree>, path: Path) -> Self {
        Self { tree, path }
    }
}

/// Appends a unit to an attribute's name.
fn with_unit(name: &str, unit: Option<&str>) -> String {
    match unit {
        Some(unit) => format!("{name}_{unit}"),
        None => name.to_string(),
    }
}
