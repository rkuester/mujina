//! Where the daemon reads and writes configuration.
//!
//! MIP-0001 UR11 has the daemon stay out of the system paths unless
//! it runs as a packaged service. This covers the developer's half
//! of that list: an explicit file, an explicit directory, and a
//! `.mujina/` directory found by walking up from the working
//! directory. The system layout, the XDG layout, and the flags that
//! ask for them are not built yet.

use std::env;
use std::path::{Path, PathBuf};

use crate::tracing::prelude::*;

/// Names one file to read and nothing to write.
const CONFIG_FILE: &str = "MUJINA_CONFIG_FILE";

/// Names the directory holding the configuration files.
const DIR: &str = "MUJINA_DIR";

/// The directory a checkout keeps its configuration in.
const DISCOVERED_DIR: &str = ".mujina";

/// The main configuration file inside a directory.
const MAIN: &str = "config.yaml";

/// The directory of drop-in configuration files.
const DROP_IN: &str = "config.d";

/// The file the daemon writes what a client sets into.
const SAVED: &str = "saved-config.yaml";

/// Where the daemon reads and writes configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Location {
    /// One file, read at startup. Nothing persists, so a client's
    /// writes last as long as the daemon runs.
    File(PathBuf),

    /// A directory holding the config files and the saved config.
    Directory(PathBuf),

    /// Nothing on disk. The daemon runs on compiled defaults, the
    /// environment, and whatever a client writes while it runs.
    Defaults,
}

impl Location {
    /// Returns the first location that resolves.
    pub fn discover() -> Self {
        if let Some(file) = env::var_os(CONFIG_FILE) {
            return Self::File(PathBuf::from(file));
        }
        if let Some(directory) = env::var_os(DIR) {
            return Self::Directory(PathBuf::from(directory));
        }
        match env::current_dir() {
            Ok(working) => Self::walk_up(&working),
            Err(e) => {
                warn!(error = %e, "cannot read the working directory");
                Self::Defaults
            }
        }
    }

    /// Logs where configuration comes from and goes to.
    ///
    /// Which location resolved decides whether a client's writes
    /// survive a restart, so say it once at startup rather than
    /// leaving it to be worked out from a missing file.
    pub fn announce(&self) {
        match self {
            Self::File(file) => info!(
                file = %file.display(),
                "Reading configuration from one file. A client's writes will not be saved."
            ),
            Self::Directory(directory) => info!(
                directory = %directory.display(),
                "Reading configuration from a directory."
            ),
            Self::Defaults => info!(
                "No configuration directory found. Running on compiled defaults \
                 and the environment; a client's writes will not be saved."
            ),
        }
    }

    /// Returns the configuration files to read, lowest first.
    ///
    /// The main file comes first and the drop-ins follow in lexical
    /// order, so a later file overrides an earlier one, which is
    /// systemd's drop-in convention.
    pub fn files(&self) -> Vec<PathBuf> {
        let directory = match self {
            Self::File(file) => return vec![file.clone()],
            Self::Directory(directory) => directory,
            Self::Defaults => return Vec::new(),
        };

        let mut files = Vec::new();
        let main = directory.join(MAIN);
        if main.is_file() {
            files.push(main);
        }

        let Ok(entries) = directory.join(DROP_IN).read_dir() else {
            return files;
        };
        let mut drop_ins: Vec<_> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|kind| kind == "yaml"))
            .collect();
        drop_ins.sort();
        files.extend(drop_ins);

        files
    }

    /// Returns the file the daemon saves a client's writes into.
    ///
    /// `None` means the writes are transient: they take effect for
    /// the running daemon and are gone at the next start.
    pub fn saved(&self) -> Option<PathBuf> {
        match self {
            Self::Directory(directory) => Some(directory.join(SAVED)),
            Self::File(_) | Self::Defaults => None,
        }
    }

    /// Returns the first `.mujina` directory at or above a directory.
    fn walk_up(from: &Path) -> Self {
        for directory in from.ancestors() {
            let candidate = directory.join(DISCOVERED_DIR);
            if candidate.is_dir() {
                return Self::Directory(candidate);
            }
        }
        Self::Defaults
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_single_file_saves_nothing() {
        let location = Location::File(PathBuf::from("/tmp/one.yaml"));

        assert_eq!(location.files(), vec![PathBuf::from("/tmp/one.yaml")]);
        assert_eq!(location.saved(), None);
    }

    #[test]
    fn the_defaults_read_and_write_no_files() {
        assert!(Location::Defaults.files().is_empty());
        assert_eq!(Location::Defaults.saved(), None);
    }

    #[test]
    fn a_directory_saves_beside_its_config_files() {
        let location = Location::Directory(PathBuf::from("/etc/mujina"));

        assert_eq!(
            location.saved(),
            Some(PathBuf::from("/etc/mujina/saved-config.yaml"))
        );
    }
}
