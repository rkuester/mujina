//! Guards: group multiple writes so they take effect together.

use std::sync::Arc;

use parking_lot::RwLockWriteGuard;

use super::attribute::{Attribute, AttributeValue, Report};
use super::tree::{AttributeMap, Tree};

/// Holds the tree open for writing.
///
/// Every value written through one guard becomes visible together.
/// Subscribers are notified after the guard releases the tree.
pub struct WriteGuard<'a> {
    tree: Option<RwLockWriteGuard<'a, AttributeMap>>,
    changed: Vec<Arc<dyn Attribute>>,
}

impl<'a> WriteGuard<'a> {
    pub(super) fn open(tree: &'a Tree) -> Self {
        Self {
            tree: Some(tree.hold_for_writing()),
            changed: Vec::new(),
        }
    }
}

impl WriteGuard<'_> {
    /// Writes a value to an attribute.
    pub fn set<T: AttributeValue>(&mut self, attribute: &Report<T>, value: T) {
        if let Some(unwoken) = attribute.store(value) {
            self.changed.push(unwoken);
        }
    }

    /// Reads the current value of an attribute, including any writes made
    /// through this guard.
    pub fn get<T: AttributeValue>(&self, attribute: &Report<T>) -> T {
        attribute.get()
    }
}

impl Drop for WriteGuard<'_> {
    fn drop(&mut self) {
        // Release the tree before notifying subscribers, so a
        // subscriber that reads on wake does not block on this guard.
        drop(self.tree.take());

        for attribute in self.changed.drain(..) {
            attribute.notify();
        }
    }
}
