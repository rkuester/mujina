//! Settings: the attributes a client and a config file write.

use std::fmt::Display;
use std::ops::RangeInclusive;
use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::Value;
use tokio::sync::watch;
use tokio_stream::wrappers::WatchStream;

use super::attribute::{Attribute, AttributeKind, SettingValue, ValueError, store_quietly};
use super::resolver::Origin;

/// A handle to a single value a client sets.
///
/// It reads exactly like a [`Report`](super::Report), and it has no
/// `set`, because a component never writes its own configuration.
/// The value arrives from the layers, through the tree.
pub struct Setting<T> {
    cell: Arc<SettingCell<T>>,
}

/// Rejects a value outside a range, and accepts an unset value.
///
/// An unset setting means the daemon decides, so it is in range
/// whatever the range is.
pub fn within<T>(range: RangeInclusive<T>) -> impl Fn(&Option<T>) -> Result<(), String>
where
    T: PartialOrd + Display,
{
    move |value| match value {
        Some(value) if !range.contains(value) => Err(format!(
            "{value} lies outside {}..={}",
            range.start(),
            range.end()
        )),
        _ => Ok(()),
    }
}

impl<T: SettingValue> Setting<T> {
    /// Returns the current value in the attribute.
    pub fn get(&self) -> T {
        self.cell.value.borrow().clone()
    }

    /// Returns a receiver that wakes when the value changes.
    pub fn subscribe(&self) -> watch::Receiver<T> {
        self.cell.value.subscribe()
    }

    /// Returns a stream yielding the current value and every subsequent value.
    pub fn follow(&self) -> WatchStream<T> {
        WatchStream::new(self.cell.value.subscribe())
    }

    /// Returns the layer the current value came from.
    pub fn origin(&self) -> Origin {
        *self.cell.origin.lock()
    }

    pub(super) fn new(cell: Arc<SettingCell<T>>) -> Self {
        Self { cell }
    }
}

impl<T> Clone for Setting<T> {
    fn clone(&self) -> Self {
        Self {
            cell: self.cell.clone(),
        }
    }
}

/// What a setting attribute holds beyond its value.
///
/// The validator runs on every value that tries to enter, whatever
/// layer it came from, which is MIP-0001 UR6. The origin records
/// which layer the value in the channel came from, so the API can
/// answer where a value came from without asking the layers again.
pub(super) struct SettingCell<T> {
    value: watch::Sender<T>,
    default: T,
    check: Option<Check<T>>,
    origin: Mutex<Origin>,
}

/// Rejects a value the attribute will not hold.
pub(super) type Check<T> = Box<dyn Fn(&T) -> Result<(), String> + Send + Sync>;

impl<T: SettingValue> SettingCell<T> {
    pub(super) fn new(default: T, check: Option<Check<T>>) -> Self {
        Self {
            value: watch::channel(default.clone()).0,
            default,
            check,
            origin: Mutex::new(Origin::Builtin),
        }
    }

    /// Boxes a validator for [`new`](Self::new).
    pub(super) fn checking(
        check: impl Fn(&T) -> Result<(), String> + Send + Sync + 'static,
    ) -> Check<T> {
        Box::new(check)
    }
}

impl<T: SettingValue> Attribute for SettingCell<T> {
    fn notify(&self) {
        self.value.send_modify(|_| ());
    }

    fn to_json(&self) -> Value {
        self.value.borrow().to_json()
    }

    fn kind(&self) -> AttributeKind {
        AttributeKind::Setting
    }

    fn origin(&self) -> Option<Origin> {
        Some(*self.origin.lock())
    }

    fn store_json(&self, value: &Value, origin: Origin) -> Result<bool, ValueError> {
        let value = T::from_json(value)?;
        if let Some(check) = &self.check {
            check(&value).map_err(ValueError::Rejected)?;
        }

        // A value that came from a higher layer changes where the
        // effective value comes from, even when the value itself
        // reads the same.
        *self.origin.lock() = origin;

        Ok(store_quietly(&self.value, value))
    }

    fn reset(&self) -> bool {
        *self.origin.lock() = Origin::Builtin;
        store_quietly(&self.value, self.default.clone())
    }
}
