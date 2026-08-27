//! Tests for the tree, its cursors, and its guards.

use std::fmt;
use std::sync::Arc;
use std::thread;

use serde_json::{Value, json};
use tokio_stream::StreamExt;

use super::*;

/// Returns a cursor on the root of an empty tree.
fn root() -> Cursor {
    Tree::new(Arc::new(Unconfigured)).root()
}

/// A test type with a unit, simulating quantity types a board
/// reports.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
struct Volts(f64);

impl AttributeValue for Volts {
    const UNIT: Option<&'static str> = Some("v");

    fn to_json(&self) -> Value {
        json!(self.0)
    }
}

impl SettingValue for Volts {
    fn from_json(value: &Value) -> Result<Self, ValueError> {
        match value.as_f64() {
            Some(volts) => Ok(Self(volts)),
            None => Err(ValueError::WrongType("a voltage in volts")),
        }
    }
}

impl fmt::Display for Volts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.3} V", self.0)
    }
}

#[test]
fn an_attribute_starts_at_the_value_it_was_given() {
    let temperature = root().report("asic", Some(61.5));

    assert_eq!(temperature.get(), Some(61.5));
}

#[test]
fn a_write_replaces_the_value() {
    let root = root();
    let temperature = root.report("asic", None);

    root.write().set(&temperature, Some(61.5));

    assert_eq!(temperature.get(), Some(61.5));
}

#[test]
fn a_guard_reads_back_what_it_wrote() {
    let root = root();
    let rpm = root.report("rpm", 0u32);

    let mut fans = root.write();
    fans.set(&rpm, 4200);

    assert_eq!(fans.get(&rpm), 4200);
}

#[test]
fn subscribers_wait_for_the_whole_group() {
    let root = root();
    let voltage = root.report("voltage_uv", 0u64);
    let current = root.report("current_ma", 0u64);
    let mut on_voltage = voltage.subscribe();

    let mut rail = root.write();
    rail.set(&voltage, 1_150_000);
    rail.set(&current, 15_000);
    assert!(
        !on_voltage.has_changed().unwrap(),
        "woke before the group was finished"
    );
    drop(rail);

    assert!(on_voltage.has_changed().unwrap());
    assert_eq!(*on_voltage.borrow_and_update(), 1_150_000);
    assert_eq!(current.get(), 15_000);
    assert!(
        !on_voltage.has_changed().unwrap(),
        "woke twice for one group"
    );
}

#[test]
fn writing_the_value_that_is_already_there_wakes_nobody() {
    let root = root();
    let temperature = root.report("asic", Some(61.5));
    let watcher = temperature.subscribe();

    root.write().set(&temperature, Some(61.5));

    assert!(!watcher.has_changed().unwrap());
}

#[test]
fn subscribers_outlive_the_handle() {
    let root = root();
    let temperature = root.report("asic", Some(61.5));
    let watcher = temperature.subscribe();

    drop(temperature);

    assert_eq!(*watcher.borrow(), Some(61.5));
    assert!(
        watcher.has_changed().is_ok(),
        "the tree let go of the attribute with its handle"
    );
}

#[tokio::test]
async fn following_an_attribute_starts_at_its_current_value() {
    let root = root();
    let hashes = root.report("hashes_per_s", 0u64);
    let mut published = hashes.follow();

    assert_eq!(published.next().await, Some(0));

    root.write().set(&hashes, 1_200_000_000_000);

    assert_eq!(published.next().await, Some(1_200_000_000_000));
}

#[test]
fn a_document_nests_values_by_path() {
    let root = root();
    let board = root.child("boards").child("bitaxe-e2f56f9b");
    board.report("model", "Bitaxe Gamma".to_string());
    board.report("asic_temperature_c", Some(61.5));
    board
        .child("threads")
        .child("0")
        .report("hashes_per_s", 1_200u64);

    assert_eq!(
        root.render_json(),
        Some(json!({
            "boards": {
                "bitaxe-e2f56f9b": {
                    "model": "Bitaxe Gamma",
                    "asic_temperature_c": 61.5,
                    "threads": {"0": {"hashes_per_s": 1200}},
                }
            }
        }))
    );
}

#[test]
fn a_unit_joins_the_name_of_the_leaf() {
    let root = root();
    let regulator = root.child("regulators").child("core");
    regulator.report("voltage", Some(Volts(1.15)));
    regulator.report("target_voltage", None::<Volts>);

    assert_eq!(
        root.render_json(),
        Some(json!({
            "regulators": {
                "core": {"voltage_v": 1.15, "target_voltage_v": null}
            }
        }))
    );
}

#[test]
fn a_cursor_on_an_attribute_renders_the_value_alone() {
    let root = root();
    let regulator = root.child("regulators").child("core");
    regulator.report("voltage", Some(Volts(1.15)));

    assert_eq!(
        regulator.child("voltage_v").render_json(),
        Some(json!(1.15))
    );
}

#[test]
fn a_missing_node_and_a_null_value_stay_apart() {
    let root = root();
    let board = root.child("boards").child("x");
    board.report("temperature_c", None::<f64>);

    assert_eq!(
        board.child("temperature_c").render_json(),
        Some(Value::Null)
    );
    assert_eq!(root.child("boards").child("gone").render_json(), None);
    assert_eq!(
        Tree::new(Arc::new(Unconfigured)).root().render_json(),
        None,
        "empty tree returns None"
    );
}

#[test]
fn a_float_with_no_json_form_renders_as_null() {
    let root = root();
    root.report("efficiency", f64::NAN);

    assert_eq!(root.render_json(), Some(json!({"efficiency": null})));
}

#[test]
fn a_sibling_sharing_a_prefix_stays_out_of_the_document() {
    let root = root();
    let boards = root.child("boards");
    boards.child("x").report("hashes_per_s", 1u64);
    boards.child("x-2").report("hashes_per_s", 2u64);

    assert_eq!(
        boards.child("x").render_json(),
        Some(json!({"hashes_per_s": 1}))
    );
}

#[test]
fn a_value_where_a_branch_belongs_stays_out_of_the_document() {
    let root = root();
    root.report("boards", 1u32);
    root.child("boards").report("count", 2u32);

    assert_eq!(root.render_json(), Some(json!({"boards": 1})));
}

#[test]
fn several_readers_hold_the_tree_at_once() {
    let root = root();
    let hashes = root.report("hashes_per_s", 1_200u64);

    let first = root.read();
    let second = root.read();

    assert_eq!(first.get(&hashes), second.get(&hashes));
}

#[test]
fn a_group_of_writes_waits_for_a_reader() {
    let root = root();
    let voltage = root.report("voltage_uv", 0u64);
    let current = root.report("current_ma", 0u64);
    let reading = root.read();

    let rail = thread::spawn({
        let (root, voltage, current) = (root.clone(), voltage.clone(), current.clone());
        move || {
            let mut group = root.write();
            group.set(&voltage, 1_150_000);
            group.set(&current, 15_000);
        }
    });

    assert_eq!(
        (reading.get(&voltage), reading.get(&current)),
        (0, 0),
        "writes proceeded while a reader held the tree"
    );
    drop(reading);

    rail.join().expect("the writer finished");
    assert_eq!((voltage.get(), current.get()), (1_150_000, 15_000));
}

/// A resolver answering from a fixed list of entries.
struct Layers {
    entries: Vec<(Selector, Value)>,
}

impl Layers {
    /// Returns layers holding one entry.
    fn holding(selector: &str, value: Value) -> Arc<Self> {
        Arc::new(Self {
            entries: vec![(selector.parse().expect("a valid selector"), value)],
        })
    }
}

impl Resolver for Layers {
    fn resolve(&self, path: &Path) -> Option<(Value, Origin)> {
        self.entries
            .iter()
            .find(|(selector, _)| selector.matches(path))
            .map(|(_, value)| (value.clone(), Origin::Admin))
    }
}

/// Returns a tree over the given layers.
fn tree_over(resolver: Arc<dyn Resolver>) -> Arc<Tree> {
    Tree::new(resolver)
}

/// Parses a path that must be valid.
fn path(pointer: &str) -> Path {
    pointer.parse().expect("valid pointer")
}

#[test]
fn a_setting_no_layer_supplies_keeps_its_default() {
    let target = root().setting("target_voltage", Some(Volts(1.15)));

    assert_eq!(target.get(), Some(Volts(1.15)));
    assert_eq!(target.origin(), Origin::Builtin);
}

#[test]
fn a_setting_resolves_as_it_is_created() {
    let tree = tree_over(Layers::holding("/boards/*/target_voltage_v", json!(1.25)));

    let target = tree
        .root()
        .child("boards")
        .child("x")
        .setting("target_voltage", None::<Volts>);

    assert_eq!(target.get(), Some(Volts(1.25)));
    assert_eq!(target.origin(), Origin::Admin);
}

#[test]
fn a_configured_value_the_attribute_rejects_leaves_the_default() {
    let tree = tree_over(Layers::holding("/target_voltage_v", json!(9.0)));

    let target = tree.root().setting_checked(
        "target_voltage",
        Some(Volts(1.15)),
        within(Volts(0.0)..=Volts(2.0)),
    );

    assert_eq!(target.get(), Some(Volts(1.15)));
    assert_eq!(target.origin(), Origin::Builtin);
}

#[test]
fn a_client_write_reaches_the_setting_and_wakes_its_subscriber() {
    let tree = Tree::new(Arc::new(Unconfigured));
    let target = tree.root().setting("target_voltage", None::<Volts>);
    let watcher = target.subscribe();

    tree.store_json(&path("/target_voltage_v"), &json!(1.3), Origin::Api)
        .expect("the attribute takes the value");

    assert_eq!(target.get(), Some(Volts(1.3)));
    assert_eq!(target.origin(), Origin::Api);
    assert!(watcher.has_changed().expect("the attribute is alive"));
}

#[test]
fn a_client_cannot_write_a_value_the_daemon_reports() {
    let tree = Tree::new(Arc::new(Unconfigured));
    let temperature = tree.root().report("asic_temperature", Some(61.5));

    let refused = tree.store_json(&path("/asic_temperature"), &json!(20.0), Origin::Api);

    assert!(matches!(
        refused,
        Err(WriteError::Value(ValueError::Reported))
    ));
    assert_eq!(temperature.get(), Some(61.5));
}

#[test]
fn a_write_to_no_attribute_is_an_error_rather_than_a_new_attribute() {
    let tree = Tree::new(Arc::new(Unconfigured));

    let refused = tree.store_json(
        &path("/boards/x/target_voltage_v"),
        &json!(1.2),
        Origin::Api,
    );

    assert!(matches!(refused, Err(WriteError::NoSuchAttribute(_))));
    assert_eq!(tree.render(&Path::root()), None);
}

#[test]
fn an_attribute_rejects_a_value_outside_its_range() {
    let tree = Tree::new(Arc::new(Unconfigured));
    let target = tree.root().setting_checked(
        "target_voltage",
        Some(Volts(1.15)),
        within(Volts(1.0)..=Volts(1.4)),
    );

    let refused = tree.store_json(&path("/target_voltage_v"), &json!(2.0), Origin::Api);

    assert!(matches!(
        refused,
        Err(WriteError::Value(ValueError::Rejected(_)))
    ));
    assert_eq!(target.get(), Some(Volts(1.15)));
}

#[test]
fn an_unset_setting_is_a_value_a_client_can_ask_for() {
    let tree = Tree::new(Arc::new(Unconfigured));
    let target = tree.root().setting_checked(
        "target_voltage",
        Some(Volts(1.15)),
        within(Volts(1.0)..=Volts(1.4)),
    );

    tree.store_json(&path("/target_voltage_v"), &Value::Null, Origin::Api)
        .expect("null means the daemon decides");

    assert_eq!(target.get(), None);
}

#[test]
fn a_setting_no_layer_supplies_any_longer_returns_to_its_default() {
    let tree = Tree::new(Arc::new(Unconfigured));
    let target = tree.root().setting("target_voltage", Some(Volts(1.15)));
    tree.store_json(&path("/target_voltage_v"), &json!(1.3), Origin::Api)
        .expect("the attribute takes the value");

    tree.resolve_all();

    assert_eq!(target.get(), Some(Volts(1.15)));
    assert_eq!(target.origin(), Origin::Builtin);
}

#[test]
fn only_settings_report_where_their_value_came_from() {
    let tree = tree_over(Layers::holding("/boards/x/target_voltage_v", json!(1.25)));
    let board = tree.root().child("boards").child("x");
    board.report("asic_temperature", Some(61.5));
    board.setting("target_voltage", None::<Volts>);

    assert_eq!(
        tree.origins(&Path::root()),
        json!({"/boards/x/target_voltage_v": "admin"})
    );
}
