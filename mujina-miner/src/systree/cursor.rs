//! Cursors: a path into the tree and the ability to create attributes under it.

use std::sync::Arc;

use tokio::sync::watch;

use super::attribute::{Attribute, AttributeValue, Report};
use super::guard::WriteGuard;
use super::path::Path;
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

    /// Attaches an attribute for the caller to report, and returns its handle.
    pub fn report<T: AttributeValue>(&self, name: &str, initial: T) -> Report<T> {
        let (sender, _) = watch::channel(initial);
        let sender = Arc::new(sender);

        let attribute: Arc<dyn Attribute> = sender.clone();
        self.tree.attach(self.path.child(name), attribute);

        Report::new(sender)
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
