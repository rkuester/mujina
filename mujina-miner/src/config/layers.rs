//! The layers a setting's value can come from, and what is in each.

use serde_json::Value;

use crate::systree::{Origin, Path, Selector};

/// One selector and one value.
///
/// The value stays untyped, because an entry can arrive long before
/// the attribute that gives it a type, and may name a board that is
/// not plugged in yet.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    selector: Selector,
    value: Value,
}

/// One input to the cascade, holding what that input set.
#[derive(Debug)]
pub struct Layer {
    origin: Origin,
    entries: Vec<Entry>,
}

/// Every layer, ordered lowest to highest.
#[derive(Debug, Default)]
pub struct Layers {
    layers: Vec<Layer>,
}

impl Entry {
    /// Returns an entry naming a set of paths.
    pub fn new(selector: Selector, value: Value) -> Self {
        Self { selector, value }
    }

    /// Returns the paths the entry applies to.
    pub fn selector(&self) -> &Selector {
        &self.selector
    }

    /// Returns the value the entry sets.
    pub fn value(&self) -> &Value {
        &self.value
    }
}

impl Layer {
    /// Returns an empty layer.
    pub fn new(origin: Origin) -> Self {
        Self {
            origin,
            entries: Vec::new(),
        }
    }

    /// Returns the layer's entries.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Replaces every entry in the layer.
    pub fn replace(&mut self, entries: Vec<Entry>) {
        self.entries = entries;
    }

    /// Sets one selector's value, replacing an entry with the same
    /// selector.
    pub fn set(&mut self, selector: Selector, value: Value) {
        match self
            .entries
            .iter_mut()
            .find(|entry| entry.selector == selector)
        {
            Some(entry) => entry.value = value,
            None => self.entries.push(Entry::new(selector, value)),
        }
    }

    /// Removes one selector's entry, and reports whether it was there.
    pub fn remove(&mut self, selector: &Selector) -> bool {
        let before = self.entries.len();
        self.entries.retain(|entry| &entry.selector != selector);
        before != self.entries.len()
    }

    /// Returns the most specific entry that names a path.
    fn best_match(&self, path: &Path) -> Option<&Entry> {
        self.entries
            .iter()
            .filter(|entry| entry.selector.matches(path))
            .reduce(|best, entry| {
                if entry.selector.more_specific_than(&best.selector) {
                    entry
                } else {
                    best
                }
            })
    }
}

impl Layers {
    /// Returns the layers of the cascade, each empty.
    ///
    /// The compiled defaults are not a layer here. A default lives
    /// in the attribute that declares it, so a setting no layer
    /// supplies already holds it.
    pub fn new(origins: &[Origin]) -> Self {
        Self {
            layers: origins.iter().copied().map(Layer::new).collect(),
        }
    }

    /// Returns the value in effect for a path, and where it came from.
    ///
    /// Walks the layers from highest to lowest and takes the first
    /// hit. Specificity decides within a layer and never across
    /// layers, so a class rule in a higher layer outranks an
    /// instance value in a lower one.
    pub fn resolve(&self, path: &Path) -> Option<(Value, Origin)> {
        self.layers.iter().rev().find_map(|layer| {
            layer
                .best_match(path)
                .map(|e| (e.value.clone(), layer.origin))
        })
    }

    /// Returns the named layer.
    pub fn layer(&mut self, origin: Origin) -> &mut Layer {
        let position = self
            .layers
            .iter()
            .position(|layer| layer.origin == origin)
            .expect("every layer the daemon writes exists");
        &mut self.layers[position]
    }

    /// Returns the named layer's entries.
    pub fn entries(&self, origin: Origin) -> &[Entry] {
        self.layers
            .iter()
            .find(|layer| layer.origin == origin)
            .map(Layer::entries)
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// Parses a path that must be valid.
    fn path(pointer: &str) -> Path {
        pointer.parse().expect("valid pointer")
    }

    /// Parses a selector that must be valid.
    fn selector(pointer: &str) -> Selector {
        pointer.parse().expect("valid pointer")
    }

    /// Returns the layers the daemon builds, with nothing in them.
    fn layers() -> Layers {
        Layers::new(&[Origin::Admin, Origin::Saved, Origin::Environment])
    }

    #[test]
    fn a_higher_layer_wins_whatever_its_shape() {
        let mut layers = layers();
        layers.layer(Origin::Admin).set(
            selector("/boards/bitaxe-gamma-e2f5/target_voltage_v"),
            json!(1.1),
        );
        layers
            .layer(Origin::Saved)
            .set(selector("/boards/*/target_voltage_v"), json!(1.3));

        assert_eq!(
            layers.resolve(&path("/boards/bitaxe-gamma-e2f5/target_voltage_v")),
            Some((json!(1.3), Origin::Saved))
        );
    }

    #[test]
    fn the_most_specific_entry_wins_within_a_layer() {
        let mut layers = layers();
        let admin = layers.layer(Origin::Admin);
        admin.set(selector("/boards/*/*/target_voltage_v"), json!(1.0));
        admin.set(
            selector("/boards/*/regulators/target_voltage_v"),
            json!(1.2),
        );
        admin.set(
            selector("/boards/bitaxe-gamma-e2f5/regulators/target_voltage_v"),
            json!(1.25),
        );

        assert_eq!(
            layers
                .resolve(&path(
                    "/boards/bitaxe-gamma-e2f5/regulators/target_voltage_v"
                ))
                .map(|(value, _)| value),
            Some(json!(1.25))
        );
        assert_eq!(
            layers
                .resolve(&path(
                    "/boards/emberone-00-abcd/regulators/target_voltage_v"
                ))
                .map(|(value, _)| value),
            Some(json!(1.2))
        );
        assert_eq!(
            layers
                .resolve(&path("/boards/emberone-00-abcd/fans/target_voltage_v"))
                .map(|(value, _)| value),
            Some(json!(1.0))
        );
    }

    #[test]
    fn a_rule_for_one_model_reaches_that_model_alone() {
        let mut layers = layers();
        let admin = layers.layer(Origin::Admin);
        admin.set(selector("/boards/*/target_frequency_hz"), json!(400));
        admin.set(
            selector("/boards/emberone-00-*/target_frequency_hz"),
            json!(500),
        );

        assert_eq!(
            layers
                .resolve(&path("/boards/emberone-00-abcd/target_frequency_hz"))
                .map(|(value, _)| value),
            Some(json!(500))
        );
        assert_eq!(
            layers
                .resolve(&path("/boards/bitaxe-gamma-e2f5/target_frequency_hz"))
                .map(|(value, _)| value),
            Some(json!(400))
        );
    }

    #[test]
    fn a_path_no_layer_names_resolves_to_nothing() {
        let layers = layers();

        assert_eq!(layers.resolve(&path("/boards/x/target_voltage_v")), None);
    }

    #[test]
    fn setting_a_selector_twice_replaces_its_value() {
        let mut layers = layers();
        let saved = layers.layer(Origin::Saved);
        saved.set(selector("/boards/x/target_voltage_v"), json!(1.1));
        saved.set(selector("/boards/x/target_voltage_v"), json!(1.2));

        assert_eq!(saved.entries().len(), 1);
        assert_eq!(
            layers
                .resolve(&path("/boards/x/target_voltage_v"))
                .map(|(value, _)| value),
            Some(json!(1.2))
        );
    }

    #[test]
    fn removing_an_entry_uncovers_the_layer_beneath() {
        let mut layers = layers();
        layers
            .layer(Origin::Admin)
            .set(selector("/boards/x/target_voltage_v"), json!(1.1));
        layers
            .layer(Origin::Saved)
            .set(selector("/boards/x/target_voltage_v"), json!(1.2));

        assert!(
            layers
                .layer(Origin::Saved)
                .remove(&selector("/boards/x/target_voltage_v"))
        );

        assert_eq!(
            layers.resolve(&path("/boards/x/target_voltage_v")),
            Some((json!(1.1), Origin::Admin))
        );
    }
}
