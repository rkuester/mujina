//! What every board owes the tree.
//!
//! A board file attaches these attributes when it starts, whatever
//! its model, so the API can name a board and say whether it is
//! there without knowing one model from another.

use crate::systree::{Cursor, MemberStatus, Report, WriteGuard};

/// What a board model is called, in prose and in a path.
///
/// A board's key opens with its model because a configuration
/// selector tests a path and never a value, so a rule for a class of
/// board needs the class in the path. A selector reaches the class
/// with a wildcard, as in `/boards/emberone-00-*`. The model in the
/// key also keeps two models that ship the same serial apart.
#[derive(Clone, Copy, Debug)]
pub struct BoardModel {
    /// The name a client reads, such as `Bitaxe Gamma`.
    pub name: &'static str,

    /// The word a board's key opens with, such as `bitaxe-gamma`.
    pub key: &'static str,
}

/// The attributes every board has, whatever its model.
///
/// A model's own struct holds this one, so no board joins the tree
/// without saying what it is and whether it is there. Cloning it
/// gives another set of handles to the same attributes, so a board
/// and its monitor can each hold one.
#[derive(Clone)]
pub struct BoardAttributes {
    sys: Cursor,

    /// Whether the board is plugged in and working.
    pub status: Report<MemberStatus>,
}

impl BoardAttributes {
    /// Attaches the common board attributes under the collection of
    /// boards.
    ///
    /// A board's key is its model and its serial joined, one
    /// segment, so the collection stays a flat list of boards.
    /// Producing the serial is part of being a board; a board that
    /// cannot produce one fails to attach rather than joining the
    /// tree under a name that collides with the next such board.
    ///
    /// The model is written once, here, and its handle dropped.
    /// Nothing writes a model again, and the tree keeps the
    /// attribute.
    pub fn attach(sys_boards: &Cursor, model: BoardModel, serial: &str) -> Self {
        let sys = sys_boards.child(&format!("{}-{serial}", model.key));
        sys.report("model", model.name.to_string());

        Self {
            status: sys.report("status", MemberStatus::Present),
            sys,
        }
    }

    /// Returns a cursor on the board's own place in the tree.
    ///
    /// A model's struct builds its regulators, fans, and threads
    /// under it.
    pub fn cursor(&self) -> &Cursor {
        &self.sys
    }

    /// Opens the tree for writing.
    ///
    /// The guard covers the whole tree, not this board alone.
    /// Everything written before it drops becomes visible at one
    /// instant.
    pub fn write(&self) -> WriteGuard<'_> {
        self.sys.write()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use crate::systree::{Tree, Unconfigured};

    use super::*;

    /// The model a test board reports.
    const MODEL: BoardModel = BoardModel {
        name: "Bitaxe Gamma",
        key: "bitaxe-gamma",
    };

    /// Attaches a board and returns it with a cursor on the root.
    fn board() -> (Cursor, BoardAttributes) {
        let root = Tree::new(Arc::new(Unconfigured)).root();
        let attrs = BoardAttributes::attach(&root.child("boards"), MODEL, "e2f5");
        (root, attrs)
    }

    #[test]
    fn a_board_reports_its_model_and_its_presence() {
        let (root, _board) = board();

        assert_eq!(
            root.render_json(),
            Some(json!({
                "boards": {
                    "bitaxe-gamma-e2f5": {"model": "Bitaxe Gamma", "status": "present"}
                }
            }))
        );
    }

    #[test]
    fn a_board_that_goes_away_says_so() {
        let (root, board) = board();

        board.write().set(&board.status, MemberStatus::Absent);

        assert_eq!(
            root.child("boards")
                .child("bitaxe-gamma-e2f5")
                .child("status")
                .render_json(),
            Some(json!("absent"))
        );
    }
}
