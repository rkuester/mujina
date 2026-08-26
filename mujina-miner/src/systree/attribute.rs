//! Attributes, each holding a single value, and the handles that reference them.

use std::sync::Arc;

use serde_json::Value;
use tokio::sync::watch;
use tokio_stream::wrappers::WatchStream;

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
        let mut changed = false;
        self.value.send_if_modified(|current| {
            if *current != value {
                *current = value;
                changed = true;
            }
            // Value is stored regardless. Returning false suppresses
            // the immediate wakeup; the write guard emits it after
            // releasing the tree.
            false
        });
        changed.then(|| self.erased())
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
}
