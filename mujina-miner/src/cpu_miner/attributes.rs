//! What a CPU mining thread holds in the tree.

use crate::asic::hash_thread::ThreadAttributes;
use crate::systree::{Cursor, Setting, within};
use crate::types::Ratio;

/// The share of each second a thread spends hashing when nothing
/// asks for another.
pub const DEFAULT_DUTY: Ratio = Ratio::from_percent(50.0);

/// The narrowest duty the thread will run at.
const MIN_DUTY: Ratio = Ratio::from_percent(1.0);

/// The widest duty the thread will run at.
const MAX_DUTY: Ratio = Ratio::from_percent(100.0);

/// A CPU mining thread's attributes.
///
/// It holds what every thread owes and adds the duty a client asks
/// of it.
pub struct CpuThreadAttributes {
    /// What every hash thread reports.
    pub thread: ThreadAttributes,

    /// The share of each second the thread spends hashing.
    ///
    /// Unset means the daemon decides, which is
    /// [`DEFAULT_DUTY`]. Duty cycling keeps a development machine
    /// from running flat out, and on a cloud instance it avoids the
    /// alerts sustained load raises.
    pub target_duty: Setting<Option<Ratio>>,
}

impl CpuThreadAttributes {
    /// Attaches a CPU thread's attributes under a cursor.
    pub fn attach(sys: &Cursor) -> Self {
        Self {
            thread: ThreadAttributes::attach(sys),
            target_duty: sys.setting_checked("target_duty", None, within(MIN_DUTY..=MAX_DUTY)),
        }
    }
}
