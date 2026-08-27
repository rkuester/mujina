//! Attributes, each holding a single value, and the handles that reference them.

use std::sync::Arc;

use serde_json::Value;
use thiserror::Error;
use tokio::sync::watch;
use tokio_stream::wrappers::WatchStream;

use super::resolver::Origin;

/// A handle to a single value reported by a component.
///
/// Cloning the handle produces another handle to the same attribute. The tree
/// retains its own handle, so a component may drop its handle without
/// closing the attribute for subscribers.
///
/// No `set` method exists. Writes occur through a write guard obtained
/// from a cursor.
pub struct Report<T> {
    value: Arc<watch::Sender<T>>,
}

/// Requirements for a value stored in an attribute.
///
/// `Clone` allows a read to return a copy. `PartialEq` enables a write
/// to compare before storing, suppressing wakeups when the value is
/// unchanged. `Send` and `Sync` permit the value to cross threads.
/// `'static` excludes borrowed data, since the tree outlives the
/// components that write to it.
///
/// An implementation specifies how the value appears in the API:
/// its JSON form and unit. Quantity types stay free of API
/// knowledge, and one implementation defines the unit for every
/// attribute holding that type.
pub trait AttributeValue: Clone + PartialEq + Send + Sync + 'static {
    /// The unit of the value. It joins the attribute's name, so
    /// `voltage` becomes `voltage_v`.
    ///
    /// Use the SI symbol where the quantity has one. Where the
    /// quantity counts events over a span of time, name the span
    /// in the unit, as in `per_s` or `per_h`, so `hashes` becomes
    /// `hashes_per_s`. Use `None` for a value with no unit.
    const UNIT: Option<&'static str> = None;

    /// Converts the value to JSON in the unit given by [`UNIT`](Self::UNIT).
    fn to_json(&self) -> Value;
}

/// Requirements for a value a client or a config file may set.
///
/// A setting arrives as JSON, from a config file, an environment
/// variable, or an API write, so it reads back from the same form
/// [`AttributeValue::to_json`] writes, in the unit the name gives.
pub trait SettingValue: AttributeValue + Sized {
    /// Reads the value from the JSON form a client writes.
    fn from_json(value: &Value) -> Result<Self, ValueError>;
}

/// Why a value could not enter an attribute.
#[derive(Debug, Error)]
pub enum ValueError {
    #[error("expected {0}")]
    WrongType(&'static str),

    #[error("{0}")]
    Rejected(String),

    #[error("the daemon reports this value, so a client cannot set it")]
    Reported,
}

/// Which of MIP-0001's two kinds of value an attribute holds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttributeKind {
    /// The daemon measures or computes it, and a client reads it.
    Reported,

    /// A client sets it and the daemon obeys.
    Setting,
}

/// An attribute with its value type erased.
///
/// The tree stores every attribute under this trait, enabling a single map
/// to hold attributes of different types.
pub(super) trait Attribute: Send + Sync {
    /// Wakes subscribers of this attribute.
    fn notify(&self);

    /// Returns the attribute's current value as JSON.
    ///
    /// The caller needs no knowledge of the value's type, which
    /// lets one piece of code render a subtree of mixed types.
    fn to_json(&self) -> Value;

    /// Returns which kind of value the attribute holds.
    ///
    /// The saver reads this to keep settings and skip reports,
    /// rather than being told which paths are settings.
    fn kind(&self) -> AttributeKind;

    /// Returns the layer the current value came from, for a setting.
    fn origin(&self) -> Option<Origin>;

    /// Stores a value that arrived as JSON, from a layer or a client.
    ///
    /// Validates before storing, so a rejected value never enters the
    /// tree. Returns whether the value changed, which tells the write
    /// guard whether subscribers are owed a wakeup. A reported
    /// attribute rejects every call.
    fn store_json(&self, value: &Value, origin: Origin) -> Result<bool, ValueError>;

    /// Returns a setting to its compiled default.
    ///
    /// Called when no layer supplies the path any longer, which is
    /// what makes a delete of a saved entry uncover the default
    /// beneath it. Returns whether the value changed.
    fn reset(&self) -> bool;
}

impl<T: AttributeValue> Report<T> {
    /// Returns the current value in the attribute.
    ///
    /// Acquires no lock. A single value cannot be partially read, so a
    /// solitary read requires no guard. The returned value may reflect
    /// a group of writes still in progress, since a guard stores
    /// incrementally and notifies subscribers upon release.
    pub fn get(&self) -> T {
        self.value.borrow().clone()
    }

    /// Returns a receiver that wakes when the value changes.
    ///
    /// To observe the current value and all subsequent values, perform
    /// these steps in order: subscribe, read the current value through
    /// the receiver, then wait. A write occurring after a read but
    /// before a subscribe would not wake the receiver. Prefer
    /// [`follow`](Self::follow) when possible to avoid such mistakes.
    pub fn subscribe(&self) -> watch::Receiver<T> {
        self.value.subscribe()
    }

    /// Returns a stream yielding the current value and every subsequent value.
    ///
    /// The first await returns immediately with the value presently in
    /// the attribute, so iterating the stream processes the current value
    /// before awaiting changes.
    pub fn follow(&self) -> WatchStream<T> {
        WatchStream::new(self.value.subscribe())
    }

    pub(super) fn new(value: Arc<watch::Sender<T>>) -> Self {
        Self { value }
    }

    /// Returns a type-erased reference to this attribute.
    pub(super) fn erased(&self) -> Arc<dyn Attribute> {
        self.value.clone()
    }

    /// Stores a value in the attribute without notifying subscribers.
    ///
    /// Returns the attribute, with its type erased, when the value differs
    /// from the current value and its subscribers are owed a wakeup.
    /// The write guard aggregates those attributes and notifies their
    /// subscribers after releasing the tree.
    pub(super) fn store(&self, value: T) -> Option<Arc<dyn Attribute>> {
        store_quietly(&self.value, value).then(|| self.erased())
    }
}

impl<T> Clone for Report<T> {
    fn clone(&self) -> Self {
        Self {
            value: self.value.clone(),
        }
    }
}

impl<T: AttributeValue> Attribute for watch::Sender<T> {
    fn notify(&self) {
        self.send_modify(|_| ());
    }

    fn to_json(&self) -> Value {
        self.borrow().to_json()
    }

    fn kind(&self) -> AttributeKind {
        AttributeKind::Reported
    }

    fn origin(&self) -> Option<Origin> {
        None
    }

    fn store_json(&self, _value: &Value, _origin: Origin) -> Result<bool, ValueError> {
        Err(ValueError::Reported)
    }

    fn reset(&self) -> bool {
        false
    }
}

/// Stores a value in a watch channel without waking its subscribers.
///
/// Returns whether the value changed, so a caller can wake them once
/// the group of writes it belongs to has finished.
pub(super) fn store_quietly<T: PartialEq>(channel: &watch::Sender<T>, value: T) -> bool {
    let mut changed = false;
    channel.send_if_modified(|current| {
        if *current != value {
            *current = value;
            changed = true;
        }
        // Value is stored regardless. Returning false suppresses
        // the immediate wakeup; the write guard emits it after
        // releasing the tree.
        false
    });
    changed
}
