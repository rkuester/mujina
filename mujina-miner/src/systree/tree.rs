//! The map of every attribute, and the lock that groups writes.

use std::collections::BTreeMap;
use std::sync::Arc;

use parking_lot::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use serde_json::Value;
use thiserror::Error;

use crate::tracing::prelude::*;

use super::attribute::{Attribute, AttributeKind, ValueError};
use super::cursor::Cursor;
use super::guard::ReadGuard;
use super::path::Path;
use super::render::{render_json, render_origins};
use super::resolver::{Origin, Resolver};
use super::selector::Selector;

/// The tree holding every named value in the miner.
pub struct Tree {
    attributes: RwLock<AttributeMap>,
    resolver: Arc<dyn Resolver>,
}

/// Why a value could not be written to a path.
#[derive(Debug, Error)]
pub enum WriteError {
    #[error("no attribute at {0}")]
    NoSuchAttribute(Path),

    #[error(transparent)]
    Value(#[from] ValueError),
}

/// Every attribute, in path order, so a subtree occupies a
/// contiguous range.
pub(super) type AttributeMap = BTreeMap<Path, Arc<dyn Attribute>>;

impl Tree {
    /// Creates an empty tree whose settings resolve through a resolver.
    ///
    /// Pass [`Unconfigured`](super::Unconfigured) where there are no
    /// layers, and every setting keeps its compiled default.
    pub fn new(resolver: Arc<dyn Resolver>) -> Arc<Self> {
        Arc::new(Self {
            attributes: RwLock::new(AttributeMap::new()),
            resolver,
        })
    }

    /// Returns a cursor on the root.
    pub fn root(self: &Arc<Self>) -> Cursor {
        Cursor::new(Arc::clone(self), Path::root())
    }

    /// Returns the subtree at a path as one JSON document.
    ///
    /// Returns `None` when no attribute lies at or under the path.
    pub fn render(&self, path: &Path) -> Option<Value> {
        let attributes = ReadGuard::open(self);
        render_json(attributes.attributes(), path)
    }

    /// Returns the layer each setting under a path came from.
    ///
    /// Reported values have no origin, so the document holds only
    /// the settings, keyed by their full paths.
    pub fn origins(&self, path: &Path) -> Value {
        let attributes = ReadGuard::open(self);
        render_origins(attributes.attributes(), path)
    }

    /// Writes a value that arrived as JSON to one attribute.
    ///
    /// The attribute validates before it stores, so a rejected value
    /// never enters the tree and the caller can persist only what was
    /// accepted.
    pub fn store_json(&self, path: &Path, value: &Value, origin: Origin) -> Result<(), WriteError> {
        let attributes = self.hold_for_writing();
        let attribute = attributes
            .get(path)
            .ok_or_else(|| WriteError::NoSuchAttribute(path.clone()))?
            .clone();
        let changed = attribute.store_json(value, origin)?;

        drop(attributes);
        if changed {
            attribute.notify();
        }
        Ok(())
    }

    /// Rewrites every setting a selector names from the layers.
    ///
    /// Call it when a layer changes. A setting no layer supplies any
    /// longer returns to its compiled default, which is what makes a
    /// delete of a saved entry uncover the layer beneath it.
    pub fn resolve_matching(&self, selector: &Selector) {
        self.resolve(Some(selector));
    }

    /// Rewrites every setting in the tree from the layers.
    ///
    /// A reload replaces a whole layer, so which paths it touched is
    /// no longer known and every setting resolves again.
    pub fn resolve_all(&self) {
        self.resolve(None);
    }

    /// Rewrites the settings a selector names, or every setting.
    fn resolve(&self, selector: Option<&Selector>) {
        let settings: Vec<_> = {
            let attributes = self.hold_for_reading();
            attributes
                .iter()
                .filter(|(path, attribute)| {
                    attribute.kind() == AttributeKind::Setting
                        && selector.is_none_or(|selector| selector.matches(path))
                })
                .map(|(path, attribute)| (path.clone(), attribute.clone()))
                .collect()
        };

        let mut changed = Vec::new();
        let attributes = self.hold_for_writing();
        for (path, attribute) in settings {
            let stored = match self.resolver.resolve(&path) {
                Some((value, origin)) => attribute.store_json(&value, origin),
                None => Ok(attribute.reset()),
            };
            match stored {
                Ok(true) => changed.push(attribute),
                Ok(false) => {}
                Err(e) => warn!(%path, error = %e, "rejecting a configured value"),
            }
        }

        // Release the tree before waking subscribers, so one that
        // reads on wake does not block on this write.
        drop(attributes);
        for attribute in changed {
            attribute.notify();
        }
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

    /// Returns what answers for a setting's value.
    pub(super) fn resolver(&self) -> &dyn Resolver {
        self.resolver.as_ref()
    }

    /// Acquires the read lock on the tree.
    pub(super) fn hold_for_reading(&self) -> RwLockReadGuard<'_, AttributeMap> {
        self.attributes.read()
    }

    /// Acquires the write lock on the entire tree.
    pub(super) fn hold_for_writing(&self) -> RwLockWriteGuard<'_, AttributeMap> {
        self.attributes.write()
    }
}
