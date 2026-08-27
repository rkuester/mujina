//! The YAML form of a configuration file.
//!
//! A document mirrors the tree. Each mapping key is a path segment,
//! and each scalar is the value of the setting at the path its keys
//! spell out. A key may hold `*`, which stands for any name, so one
//! entry can name the same setting on every board of a model.
//!
//! ```yaml
//! boards:
//!   bitaxe-gamma-e2f56f9b:
//!     regulators:
//!       core:
//!         target_voltage_v: 1.2
//!   emberone-00-*:
//!     threads:
//!       "*":
//!         target_frequency_hz: 500000000
//! ```

use anyhow::{Context as _, Result};
use serde_json::{Map, Value};

use crate::systree::{Path, Selector, leaves};

use super::layers::Entry;

/// Reads a document into the entries it holds.
pub fn read(text: &str) -> Result<Vec<Entry>> {
    let document: Value =
        serde_yaml_ng::from_str(text).context("failed to parse the configuration document")?;

    Ok(match document {
        Value::Null => Vec::new(),
        document => leaves(&document, &Path::root())
            .into_iter()
            .map(|(path, value)| Entry::new(Selector::from(&path), value))
            .collect(),
    })
}

/// Writes entries as a document.
pub fn write(entries: &[Entry]) -> Result<String> {
    serde_yaml_ng::to_string(&nest(entries)).context("failed to write the configuration document")
}

/// Nests entries back into one document.
fn nest(entries: &[Entry]) -> Value {
    let mut document = Value::Object(Map::new());

    for entry in entries {
        let selector = entry.selector().to_string();
        let Ok(path) = selector.parse::<Path>() else {
            continue;
        };

        let mut node = &mut document;
        let (name, branches) = match path.segments().split_last() {
            Some(split) => split,
            None => continue,
        };
        for segment in branches {
            node = node
                .as_object_mut()
                .expect("a branch built here is an object")
                .entry(segment)
                .or_insert_with(|| Value::Object(Map::new()));
        }
        if let Some(members) = node.as_object_mut() {
            members.insert(name.clone(), entry.value().clone());
        }
    }

    document
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_nested_mapping_names_a_path_per_scalar() {
        let entries = read(
            "\
boards:
  bitaxe-gamma-e2f5:
    regulators:
      core:
        target_voltage_v: 1.2
    fans:
      \"0\":
        target_duty_percent: 60
",
        )
        .expect("a valid document");

        let named: Vec<_> = entries
            .iter()
            .map(|entry| (entry.selector().to_string(), entry.value().clone()))
            .collect();
        assert_eq!(
            named,
            vec![
                (
                    "/boards/bitaxe-gamma-e2f5/fans/0/target_duty_percent".to_string(),
                    json!(60)
                ),
                (
                    "/boards/bitaxe-gamma-e2f5/regulators/core/target_voltage_v".to_string(),
                    json!(1.2)
                ),
            ]
        );
    }

    #[test]
    fn a_wildcard_key_names_every_member() {
        let entries = read(
            "\
boards:
  \"*\":
    threads:
      \"*\":
        target_duty_percent: 40
",
        )
        .expect("a valid document");

        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].selector().to_string(),
            "/boards/*/threads/*/target_duty_percent"
        );
        assert!(!entries[0].selector().is_exact());
    }

    #[test]
    fn null_is_a_value_of_its_own() {
        let entries =
            read("boards:\n  x:\n    target_voltage_v: null\n").expect("a valid document");

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].value(), &Value::Null);
    }

    #[test]
    fn an_empty_document_holds_no_entries() {
        assert!(read("").expect("a valid document").is_empty());
        assert!(
            read("# only a comment\n")
                .expect("a valid document")
                .is_empty()
        );
    }

    #[test]
    fn entries_survive_a_round_trip() {
        let text = "\
boards:
  cpu-miner-0:
    threads:
      \"*\":
        target_duty_percent: 40
";
        let entries = read(text).expect("a valid document");

        let written = write(&entries).expect("a document");

        assert_eq!(read(&written).expect("a valid document"), entries);
    }
}
