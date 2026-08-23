//! The map of every attribute, and the lock that groups writes.

use std::collections::BTreeMap;
use std::sync::Arc;

use parking_lot::{RwLock, RwLockWriteGuard};

use crate::tracing::prelude::*;

use super::attribute::Attribute;
use super::cursor::Cursor;
use super::path::Path;

/// The tree holding every named value in the miner.
pub struct Tree {
    attributes: RwLock<AttributeMap>,
}

/// Every attribute, in path order, so a subtree occupies a
/// contiguous range.
pub(super) type AttributeMap = BTreeMap<Path, Arc<dyn Attribute>>;

impl Tree {
    /// Creates an empty tree.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            attributes: RwLock::new(AttributeMap::new()),
        })
    }

    /// Returns a cursor on the root.
    pub fn root(self: &Arc<Self>) -> Cursor {
        Cursor::new(Arc::clone(self), Path::root())
    }

    /// Inserts an attribute at a path, replacing any existing attribute.
    pub(super) fn attach(&self, path: Path, attribute: Arc<dyn Attribute>) {
        if self
            .attributes
            .write()
            .insert(path.clone(), attribute)
            .is_some()
        {
            warn!(%path, "attaching an attribute where one already exists");
        }
    }

    /// Acquires the write lock on the entire tree.
    pub(super) fn hold_for_writing(&self) -> RwLockWriteGuard<'_, AttributeMap> {
        self.attributes.write()
    }
}
