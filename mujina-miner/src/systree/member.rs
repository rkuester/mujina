//! Membership in a collection of boards, threads, or sources.
//!
//! Every member of a collection reports whether it is there, so a
//! member named in a config file is visible before it appears, and
//! stays visible after it goes.

use serde_json::Value;

use super::attribute::AttributeValue;

/// Whether a member of a collection is there.
///
/// A board named in a config file exists in the tree before it is
/// plugged in, and a board a client has tuned stays in the tree
/// after it is unplugged. Presence is therefore a value a member
/// reports, not the presence of a branch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemberStatus {
    /// Plugged in and answering.
    Present,

    /// Nothing there.
    Absent,

    /// There, and not working.
    Faulted,
}

impl AttributeValue for MemberStatus {
    fn to_json(&self) -> Value {
        match self {
            Self::Present => "present",
            Self::Absent => "absent",
            Self::Faulted => "faulted",
        }
        .into()
    }
}
