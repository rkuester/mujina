//! How Rust types appear in the tree.
//!
//! Quantity types state their own unit and JSON form at the point
//! of definition. The types below need no unit and render as
//! themselves.

use serde_json::Value;

use super::attribute::AttributeValue;

/// Implements [`AttributeValue`] for types that serde_json renders
/// natively.
///
/// A float that is not finite has no JSON form, and renders as
/// null.
macro_rules! renders_as_itself {
    ($($type:ty),* $(,)?) => {
        $(
            impl AttributeValue for $type {
                fn to_json(&self) -> Value {
                    Value::from(self.clone())
                }
            }
        )*
    };
}

renders_as_itself!(bool, u8, u16, u32, u64, i8, i16, i32, i64, f32, f64, String);

/// An unmeasured value or unset setting renders as null.
///
/// It inherits the unit from the wrapped type, so a missing
/// voltage still attaches at `voltage_v`.
impl<T: AttributeValue> AttributeValue for Option<T> {
    const UNIT: Option<&'static str> = T::UNIT;

    fn to_json(&self) -> Value {
        match self {
            Some(value) => value.to_json(),
            None => Value::Null,
        }
    }
}
