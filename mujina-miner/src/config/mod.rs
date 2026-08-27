//! Configuration: the layers a setting's value comes from.
//!
//! The layers hold what was set and the systree holds what is in
//! effect. A setting attribute asks this component for its value as
//! it is created, and again whenever a layer changes, so no
//! component reads a layer and no layer holds a handle.
//!
//! MIP-0001 UR3 fixes the layers and their order. What is built
//! here is the admin files, the saved config, and the environment;
//! the package and runtime file layers and the command line wait
//! for the flags that name them.

mod document;
mod environment;
mod layers;
pub mod legacy;
mod location;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::systree::{Origin, Path, Resolver, Selector, Tree, WriteError};
use crate::tracing::prelude::*;

use layers::{Entry, Layers};
use parking_lot::RwLock;

pub use location::Location;

/// Every configuration layer, and the file that keeps what a client
/// writes.
pub struct Configuration {
    layers: RwLock<Layers>,
    location: Location,
    saved_file: Option<PathBuf>,
    save_requested: Notify,
}

/// How long a burst of writes collects before the file is written.
const DEBOUNCE: Duration = Duration::from_millis(500);

/// The layers this component fills, lowest to highest.
///
/// A client's write goes into two of them. The saved layer keeps it
/// across restarts, and the API layer holds it above every other
/// layer for as long as the daemon runs, so a reload cannot take
/// back what a client just asked for.
const LAYERS: &[Origin] = &[
    Origin::Admin,
    Origin::Saved,
    Origin::Environment,
    Origin::Api,
];

impl Configuration {
    /// Reads every layer and returns the configuration.
    ///
    /// Loading happens before any transport starts, so the first
    /// board to attach resolves against a full stack.
    pub fn load(location: Location) -> Arc<Self> {
        location.announce();

        let configuration = Self {
            layers: RwLock::new(Layers::new(LAYERS)),
            saved_file: location.saved(),
            location,
            save_requested: Notify::new(),
        };

        {
            let mut layers = configuration.layers.write();
            layers
                .layer(Origin::Admin)
                .replace(read_files(&configuration.location));
            layers
                .layer(Origin::Saved)
                .replace(read_saved(configuration.saved_file.as_deref()));
            layers
                .layer(Origin::Environment)
                .replace(environment::read());
        }

        Arc::new(configuration)
    }

    /// Stores what a client wrote, and keeps it across restarts.
    ///
    /// A selector naming one path is checked against that attribute
    /// first, so a rejected value never reaches a layer. A selector
    /// holding a wildcard is a standing rule that applies to every
    /// matching path, including paths created tomorrow, so it goes
    /// into the layers and each attribute takes what it will.
    pub fn store(&self, tree: &Tree, selector: Selector, value: Value) -> Result<(), WriteError> {
        if let Some(path) = selector.as_path() {
            tree.store_json(&path, &value, Origin::Api)?;
        }

        self.remember(selector.clone(), value);
        tree.resolve_matching(&selector);
        Ok(())
    }

    /// Forgets what a client wrote, so the layer beneath takes over.
    ///
    /// Reports whether either of a client's layers held the entry.
    pub fn forget(&self, tree: &Tree, selector: &Selector) -> bool {
        let removed = {
            let mut layers = self.layers.write();
            let saved = layers.layer(Origin::Saved).remove(selector);
            let live = layers.layer(Origin::Api).remove(selector);
            saved || live
        };

        if removed {
            self.save_requested.notify_one();
            tree.resolve_matching(selector);
        }
        removed
    }

    /// Re-reads the configuration files and applies what changed.
    ///
    /// Only the file layers are re-read. The environment is captured
    /// once at startup, and saved config is always current, since a
    /// client's writes go straight into it.
    pub fn reload(&self, tree: &Tree) {
        let entries = read_files(&self.location);
        info!(entries = entries.len(), "Reloading configuration files.");
        self.layers.write().layer(Origin::Admin).replace(entries);
        tree.resolve_all();
    }

    /// Writes saved config to its file until shutdown.
    ///
    /// A burst of writes waits out one debounce and costs one file
    /// write. Shutdown writes whatever is still pending.
    pub async fn save_until(self: Arc<Self>, shutdown: CancellationToken) {
        if self.saved_file.is_none() {
            debug!("Saved configuration is transient; nothing will be written.");
            return;
        }

        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = self.save_requested.notified() => {}
            }

            tokio::time::sleep(DEBOUNCE).await;
            self.save().await;
        }

        // A write that arrived inside the last debounce still has to
        // reach the file.
        self.save().await;
    }

    /// Records an entry in both of a client's layers.
    ///
    /// The saved layer keeps the value across restarts. The API
    /// layer keeps it in effect for this run whatever a file reload
    /// or an environment variable says.
    fn remember(&self, selector: Selector, value: Value) {
        {
            let mut layers = self.layers.write();
            layers
                .layer(Origin::Api)
                .set(selector.clone(), value.clone());
            layers.layer(Origin::Saved).set(selector, value);
        }
        self.save_requested.notify_one();
    }

    /// Writes saved config to its file.
    async fn save(&self) {
        let Some(file) = &self.saved_file else {
            return;
        };

        let text = {
            let layers = self.layers.read();
            match document::write(layers.entries(Origin::Saved)) {
                Ok(text) => text,
                Err(e) => {
                    error!(error = %e, "cannot write the saved configuration");
                    return;
                }
            }
        };

        // Write beside the file and rename, so a crash mid-write
        // leaves the old file whole.
        let staged = file.with_extension("yaml.new");
        if let Err(e) = tokio::fs::write(&staged, &text).await {
            error!(file = %staged.display(), error = %e, "cannot write the saved configuration");
            return;
        }
        match tokio::fs::rename(&staged, file).await {
            Ok(()) => debug!(file = %file.display(), "Saved configuration written."),
            Err(e) => {
                error!(file = %file.display(), error = %e, "cannot replace the saved configuration")
            }
        }
    }
}

impl Resolver for Configuration {
    fn resolve(&self, path: &Path) -> Option<(Value, Origin)> {
        self.layers.read().resolve(path)
    }
}

/// Reads every configuration file the location names.
fn read_files(location: &Location) -> Vec<Entry> {
    let mut entries = Vec::new();

    for file in location.files() {
        match std::fs::read_to_string(&file) {
            Ok(text) => match document::read(&text) {
                Ok(read) => {
                    info!(file = %file.display(), entries = read.len(), "Read a configuration file.");
                    entries.extend(read);
                }
                Err(e) => {
                    error!(file = %file.display(), error = %e, "ignoring an unreadable configuration file")
                }
            },
            Err(e) => {
                error!(file = %file.display(), error = %e, "cannot read a configuration file")
            }
        }
    }

    entries
}

/// Reads saved config, treating a missing file as an empty layer.
fn read_saved(file: Option<&std::path::Path>) -> Vec<Entry> {
    let Some(file) = file else {
        return Vec::new();
    };

    let text = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            error!(file = %file.display(), error = %e, "cannot read the saved configuration");
            return Vec::new();
        }
    };

    match document::read(&text) {
        Ok(entries) => {
            info!(file = %file.display(), entries = entries.len(), "Read the saved configuration.");
            entries
        }
        Err(e) => {
            error!(file = %file.display(), error = %e, "ignoring an unreadable saved configuration");
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::json;

    use crate::systree::{Cursor, Origin, Setting, Tree, within};

    use super::*;

    /// A directory of its own for each test that writes files.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let directory = std::env::temp_dir().join(format!(
                "mujina-config-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&directory).expect("a scratch directory");
            Self(directory)
        }

        /// Writes the main configuration file.
        fn config(&self, text: &str) {
            std::fs::write(self.0.join("config.yaml"), text).expect("a written file");
        }

        fn location(&self) -> Location {
            Location::Directory(self.0.clone())
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A file setting the core voltage of every board.
    ///
    /// No compiled-in environment variable names this path, so an
    /// environment a test elsewhere leaves behind cannot reach it.
    fn a_rule_for_every_board(volts: f64) -> String {
        format!(
            "boards:\n  \"*\":\n    regulators:\n      \
             core:\n        target_voltage_v: {volts}\n"
        )
    }

    /// Attaches the setting the test files name.
    fn attach(sys: &Cursor) -> Setting<Option<f64>> {
        sys.child("boards")
            .child("bitaxe-gamma-e2f5")
            .child("regulators")
            .child("core")
            .setting_checked("target_voltage_v", None, within(1.0..=1.3))
    }

    /// The path that setting lives at.
    fn selector() -> Selector {
        "/boards/bitaxe-gamma-e2f5/regulators/core/target_voltage_v"
            .parse()
            .expect("a valid selector")
    }

    #[test]
    fn a_file_reaches_a_setting_created_afterwards() {
        let scratch = Scratch::new();
        scratch.config(&a_rule_for_every_board(1.1));
        let configuration = Configuration::load(scratch.location());
        let tree = Tree::new(configuration);

        let target = attach(&tree.root());

        assert_eq!(target.get(), Some(1.1));
        assert_eq!(target.origin(), Origin::Admin);
    }

    #[test]
    fn what_a_client_wrote_outlives_a_reload() {
        let scratch = Scratch::new();
        scratch.config(&a_rule_for_every_board(1.1));
        let configuration = Configuration::load(scratch.location());
        let tree = Tree::new(configuration.clone());
        let target = attach(&tree.root());
        configuration
            .store(&tree, selector(), json!(1.2))
            .expect("the attribute takes the value");

        scratch.config(&a_rule_for_every_board(1.25));
        configuration.reload(&tree);

        assert_eq!(target.get(), Some(1.2));
        assert_eq!(target.origin(), Origin::Api);
    }

    #[test]
    fn forgetting_what_a_client_wrote_uncovers_the_file() {
        let scratch = Scratch::new();
        scratch.config(&a_rule_for_every_board(1.1));
        let configuration = Configuration::load(scratch.location());
        let tree = Tree::new(configuration.clone());
        let target = attach(&tree.root());
        configuration
            .store(&tree, selector(), json!(1.2))
            .expect("the attribute takes the value");

        assert!(configuration.forget(&tree, &selector()));

        assert_eq!(target.get(), Some(1.1));
        assert_eq!(target.origin(), Origin::Admin);
    }

    #[test]
    fn a_value_the_attribute_refuses_never_reaches_a_layer() {
        let scratch = Scratch::new();
        let configuration = Configuration::load(scratch.location());
        let tree = Tree::new(configuration.clone());
        let target = attach(&tree.root());

        let refused = configuration.store(&tree, selector(), json!(2.0));

        assert!(refused.is_err());
        assert_eq!(target.get(), None);
        assert_eq!(configuration.resolve(&selector().as_path().unwrap()), None);
    }

    #[tokio::test]
    async fn what_a_client_writes_comes_back_after_a_restart() {
        let scratch = Scratch::new();
        let configuration = Configuration::load(scratch.location());
        let tree = Tree::new(configuration.clone());
        attach(&tree.root());
        let saver = tokio::spawn(
            configuration
                .clone()
                .save_until(CancellationToken::new().child_token()),
        );

        configuration
            .store(&tree, selector(), json!(1.2))
            .expect("the attribute takes the value");
        tokio::time::sleep(DEBOUNCE * 3).await;
        saver.abort();

        let restarted = Configuration::load(scratch.location());
        let tree = Tree::new(restarted);
        let target = attach(&tree.root());
        assert_eq!(target.get(), Some(1.2));
        assert_eq!(target.origin(), Origin::Saved);
    }
}
