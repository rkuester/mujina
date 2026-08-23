//! Tests for the tree, its cursors, and its guards.

use tokio_stream::StreamExt;

use super::*;

/// Returns a cursor on the root of an empty tree.
fn root() -> Cursor {
    Tree::new().root()
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
