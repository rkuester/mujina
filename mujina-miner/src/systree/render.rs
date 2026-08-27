//! Turns a subtree into one JSON document.

use serde_json::{Map, Value};

use crate::tracing::prelude::*;

use super::path::Path;
use super::tree::AttributeMap;

/// Splits a JSON document into the path and value of each scalar.
///
/// The inverse of [`render_json`], and what turns a document a
/// client sends into the writes it asks for. A mapping is a branch
/// and anything else is a value, so a value that is itself a
/// mapping cannot be written this way. No attribute holds one.
pub fn leaves(document: &Value, prefix: &Path) -> Vec<(Path, Value)> {
    let mut found = Vec::new();
    split(document, prefix, &mut found);
    found
}

/// Collects each scalar in a document with the path its keys spell.
fn split(value: &Value, path: &Path, found: &mut Vec<(Path, Value)>) {
    match value {
        Value::Object(members) if !members.is_empty() => {
            for (name, value) in members {
                split(value, &path.child(name), found);
            }
        }
        value => found.push((path.clone(), value.clone())),
    }
}

/// Renders attributes at or under a prefix as one JSON document.
///
/// Returns `None` when no attribute exists at the path, keeping a
/// missing node distinct from an attribute holding null. A prefix that
/// names an attribute itself renders that value alone. This call works
/// for one value, one board, or the whole miner.
///
/// AttributeMap under a prefix occupy a contiguous range in the map, so
/// this walks that range once, opening an object for each segment
/// below the prefix. Rendering `/boards/x` with an attribute at
/// `/boards/x/threads/0/hashes_per_s` gives
/// `{"threads": {"0": {"hashes_per_s": 1200}}}`.
pub(super) fn render_json(attributes: &AttributeMap, prefix: &Path) -> Option<Value> {
    let depth = prefix.segments().len();
    let mut document = None;

    for (path, attribute) in attributes.range(prefix..) {
        if !path.starts_with(prefix) {
            break;
        }

        let Some((name, rest)) = path.segments()[depth..].split_first() else {
            // Prefix names the attribute itself, so render the value
            // as the whole document.
            document = Some(attribute.to_json());
            continue;
        };

        let node = document.get_or_insert_with(|| Value::Object(Map::new()));
        if !place(node, name, rest, attribute.to_json()) {
            warn!(%path, "omitting a value whose path crosses another value");
        }
    }

    document
}

/// Returns the layer each setting at or under a prefix came from.
///
/// The document is a sidecar to the one [`render_json`] returns,
/// keyed by full path rather than nested, because it is read
/// alongside the values rather than in place of them. A reported
/// value has no origin, so it does not appear.
pub(super) fn render_origins(attributes: &AttributeMap, prefix: &Path) -> Value {
    let mut origins = Map::new();

    for (path, attribute) in attributes.range(prefix..) {
        if !path.starts_with(prefix) {
            break;
        }
        if let Some(origin) = attribute.origin() {
            origins.insert(path.to_string(), origin.as_str().into());
        }
    }

    Value::Object(origins)
}

/// Nests a value under a name, creating objects as needed.
///
/// Returns false when a value already occupies a name along the
/// path, which means one path names both a value and a branch.
/// Path order ensures the shorter path comes first, so the
/// omitted attribute is always the deeper one.
fn place(node: &mut Value, name: &str, rest: &[String], value: Value) -> bool {
    let Value::Object(children) = node else {
        return false;
    };

    let Some((next, rest)) = rest.split_first() else {
        children.insert(name.to_string(), value);
        return true;
    };

    let branch = children
        .entry(name)
        .or_insert_with(|| Value::Object(Map::new()));
    place(branch, next, rest, value)
}
