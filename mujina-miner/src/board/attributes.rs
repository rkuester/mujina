//! What every board owes the tree.
//!
//! A board file attaches these attributes when it starts, whatever
//! its model, so the API can name a board and say whether it is
//! there without knowing one model from another.

use crate::systree::{Cursor, MemberStatus, Report, WriteGuard};

/// The attributes every board has, whatever its model.
///
/// A model's own struct holds this one, so no board joins the tree
/// without saying what it is and whether it is there.
pub struct BoardAttributes {
    sys: Cursor,

    /// Whether the board is plugged in and working.
    pub status: Report<MemberStatus>,
}

impl BoardAttributes {
    /// Attaches the common board attributes under a cursor.
    ///
    /// The model is written once, here, and its handle dropped.
    /// Nothing writes a model again, and the tree keeps the
    /// attribute.
    pub fn attach(sys: &Cursor, model: &str) -> Self {
        sys.report("model", model.to_string());

        Self {
            sys: sys.clone(),
            status: sys.report("status", MemberStatus::Present),
        }
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

    /// Attaches a board at `/boards/x` and returns it with a cursor
    /// on the root.
    fn board() -> (Cursor, BoardAttributes) {
        let root = Tree::new(Arc::new(Unconfigured)).root();
        let attrs = BoardAttributes::attach(&root.child("boards").child("x"), "Bitaxe Gamma");
        (root, attrs)
    }

    #[test]
    fn a_board_reports_its_model_and_its_presence() {
        let (root, _board) = board();

        assert_eq!(
            root.render_json(),
            Some(json!({
                "boards": {"x": {"model": "Bitaxe Gamma", "status": "present"}}
            }))
        );
    }

    #[test]
    fn a_board_that_goes_away_says_so() {
        let (root, board) = board();

        board.write().set(&board.status, MemberStatus::Absent);

        assert_eq!(
            root.child("boards")
                .child("x")
                .child("status")
                .render_json(),
            Some(json!("absent"))
        );
    }
}
