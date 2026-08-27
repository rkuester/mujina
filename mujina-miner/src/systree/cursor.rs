//! Cursors: a path into the tree and the ability to create attributes under it.

use std::sync::Arc;

use serde_json::Value;
use tokio::sync::watch;

use crate::tracing::prelude::*;

use super::attribute::{Attribute, AttributeValue, Report, SettingValue};
use super::guard::{ReadGuard, WriteGuard};
use super::path::Path;
use super::setting::{Check, Setting, SettingCell};
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

    /// Attaches an attribute a client sets and returns its handle.
    ///
    /// The setting resolves from the layers as it is created, so the
    /// component's first read already sees the configured value and
    /// nothing has to wait for a second pass. A value no layer
    /// supplies keeps the default given here.
    pub fn setting<T: SettingValue>(&self, name: &str, default: T) -> Setting<T> {
        self.attach_setting(name, default, None)
    }

    /// Attaches a setting that rejects the values a validator refuses.
    ///
    /// The validator runs on every value that tries to enter, whatever
    /// layer it came from, which is what MIP-0001 UR6 asks for.
    pub fn setting_checked<T: SettingValue>(
        &self,
        name: &str,
        default: T,
        check: impl Fn(&T) -> Result<(), String> + Send + Sync + 'static,
    ) -> Setting<T> {
        self.attach_setting(name, default, Some(SettingCell::checking(check)))
    }

    /// Returns the subtree under this cursor as JSON.
    ///
    /// Returns `None` when no attribute lies at or under this path,
    /// keeping a missing node distinct from an attribute holding null.
    ///
    /// Holds the tree's read lock, so a render never returns a
    /// torn document.
    pub fn render_json(&self) -> Option<Value> {
        self.tree.render(&self.path)
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

    fn attach_setting<T: SettingValue>(
        &self,
        name: &str,
        default: T,
        check: Option<Check<T>>,
    ) -> Setting<T> {
        let path = self.path.child(&with_unit(name, T::UNIT));
        let cell = Arc::new(SettingCell::new(default, check));

        if let Some((value, origin)) = self.tree.resolver().resolve(&path)
            && let Err(e) = Attribute::store_json(cell.as_ref(), &value, origin)
        {
            warn!(%path, error = %e, "rejecting a configured value");
        }

        self.tree.attach(path, cell.clone());
        Setting::new(cell)
    }
}

/// Appends a unit to an attribute's name.
fn with_unit(name: &str, unit: Option<&str>) -> String {
    match unit {
        Some(unit) => format!("{name}_{unit}"),
        None => name.to_string(),
    }
}
