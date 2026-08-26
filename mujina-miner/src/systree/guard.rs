//! Guards: group writes and reads so each sees one instant of the tree.

use std::sync::Arc;

use parking_lot::{RwLockReadGuard, RwLockWriteGuard};

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

/// Holds the tree open for reading.
///
/// Every value read through one guard comes from one instant in
/// time. Multiple readers hold the tree at once, so one render
/// never waits for another.
pub struct ReadGuard<'a> {
    tree: RwLockReadGuard<'a, AttributeMap>,
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

impl<'a> ReadGuard<'a> {
    pub(super) fn open(tree: &'a Tree) -> Self {
        Self {
            tree: tree.hold_for_reading(),
        }
    }
}

impl ReadGuard<'_> {
    /// Returns the current value of an attribute.
    pub fn get<T: AttributeValue>(&self, attribute: &Report<T>) -> T {
        attribute.get()
    }

    /// Lends the attributes the guard holds.
    pub(super) fn attributes(&self) -> &AttributeMap {
        &self.tree
    }
}
