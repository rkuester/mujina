//! The environment variables that set a path in the tree.
//!
//! MIP-0001 UR3 makes the environment a layer of the cascade and
//! leaves the mapping from a variable's name to a path open. Until a
//! convention settles, the table below names each variable and the
//! paths it sets.

use serde_json::Value;

use crate::systree::Selector;
use crate::tracing::prelude::*;

use super::layers::Entry;

/// Each variable and the paths it sets.
///
/// Only CPU threads carry a duty, so a selector over every board's
/// threads reaches exactly the threads the variable means.
const VARIABLES: &[(&str, &str)] = &[(
    "MUJINA_CPUMINER_DUTY",
    "/boards/*/threads/*/target_duty_percent",
)];

/// Reads the environment into entries.
pub fn read() -> Vec<Entry> {
    VARIABLES
        .iter()
        .filter_map(|(variable, selector)| {
            let text = std::env::var(variable).ok()?;
            let selector: Selector = selector.parse().expect("a selector written here is valid");
            match scalar(&text) {
                Ok(value) => Some(Entry::new(selector, value)),
                Err(e) => {
                    warn!(%variable, error = %e, "ignoring an unreadable environment variable");
                    None
                }
            }
        })
        .collect()
}

/// Reads one variable's text as a JSON value.
///
/// YAML's scalar rules give `40` a number, `true` a boolean, `null`
/// nothing, and anything else a string, without asking the caller to
/// quote what it means.
fn scalar(text: &str) -> Result<Value, serde_yaml_ng::Error> {
    serde_yaml_ng::from_str(text)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_variable_reads_as_the_kind_of_value_it_looks_like() {
        assert_eq!(scalar("40").unwrap(), json!(40));
        assert_eq!(scalar("1.15").unwrap(), json!(1.15));
        assert_eq!(scalar("true").unwrap(), json!(true));
        assert_eq!(scalar("null").unwrap(), json!(null));
        assert_eq!(
            scalar("stratum+tcp://pool:3333").unwrap(),
            json!("stratum+tcp://pool:3333")
        );
    }
}
