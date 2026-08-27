//! What a fan holds in the tree.

use crate::systree::{Cursor, Report, Setting, within};
use crate::types::Ratio;

/// The slowest a fan may be asked to turn.
const MIN_DUTY: Ratio = Ratio::from_percent(0.0);

/// The fastest a fan may be asked to turn.
const MAX_DUTY: Ratio = Ratio::from_percent(100.0);

/// What a fan holds in the tree.
///
/// The duty a client asks for sits beside the duty the controller
/// reports and the speed the fan actually turns.
pub struct FanAttributes {
    /// How fast the fan turns.
    pub rpm: Report<Option<u32>>,

    /// The share of full power the controller drives the fan at.
    pub duty: Report<Option<Ratio>>,

    /// The share of full power a client asks for.
    ///
    /// Unset means the daemon decides. There is no thermal control
    /// loop yet, so the daemon decides on full power.
    pub target_duty: Setting<Option<Ratio>>,
}

impl FanAttributes {
    /// Attaches a fan's attributes under a cursor.
    pub fn attach(sys: &Cursor) -> Self {
        Self {
            rpm: sys.report("rpm", None),
            duty: sys.report("duty", None),
            target_duty: sys.setting_checked("target_duty", None, within(MIN_DUTY..=MAX_DUTY)),
        }
    }
}
