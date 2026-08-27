//! Selectors: a path pattern that names a set of attributes.

use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

use super::path::{Path, PathError};

/// The set of paths a configuration entry applies to.
///
/// A selector is a path whose segments may hold `*`. A segment that
/// is only `*` matches any one name, so
/// `/boards/*/threads/*/target_duty_percent` names that setting on
/// every thread of every board. A `*` beside other text matches any
/// run of characters within that one segment, so
/// `/boards/emberone-00-*/threads/*/target_frequency_hz` names that
/// setting on every EmberOne and no other board. A selector with no
/// `*` at all names one attribute.
///
/// A selector tests a path and never a value, so whatever picks a
/// class of member has to be in the path. That is why a board's key
/// opens with its model.
///
/// Because `*` is the wildcard everywhere, a selector cannot name a
/// segment that holds a literal `*`. No key or attribute name does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selector {
    segments: Vec<Segment>,
}

/// One segment of a selector.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Segment {
    /// Matches this name and no other.
    Name(String),

    /// Matches the names this pattern of literals and `*` spells.
    Pattern(String),

    /// Matches any one name.
    Any,
}

/// The wildcard, alone as a segment or inside one.
const ANY: &str = "*";

impl Selector {
    /// Returns the selector that names one path and nothing else.
    pub fn exactly(path: &Path) -> Self {
        Self {
            segments: path
                .segments()
                .iter()
                .map(|name| Segment::Name(name.clone()))
                .collect(),
        }
    }

    /// Reports whether this selector names the path.
    pub fn matches(&self, path: &Path) -> bool {
        let names = path.segments();
        if names.len() != self.segments.len() {
            return false;
        }
        self.segments
            .iter()
            .zip(names)
            .all(|(segment, name)| match segment {
                Segment::Name(expected) => expected == name,
                Segment::Pattern(pattern) => matches_pattern(pattern, name),
                Segment::Any => true,
            })
    }

    /// Reports whether this selector is more specific than another.
    ///
    /// Compare segment by segment and let the leftmost difference
    /// decide. A whole name beats a pattern, a pattern beats a bare
    /// wildcard, and between two patterns the one that spells out
    /// more characters wins. Two selectors of different lengths
    /// never match the same path, so the comparison only has to
    /// order the ones that do.
    pub fn more_specific_than(&self, other: &Self) -> bool {
        for (mine, theirs) in self.segments.iter().zip(&other.segments) {
            match mine.specificity().cmp(&theirs.specificity()) {
                Ordering::Equal => continue,
                Ordering::Greater => return true,
                Ordering::Less => return false,
            }
        }
        false
    }

    /// Reports whether the selector names one path rather than a class.
    pub fn is_exact(&self) -> bool {
        self.segments
            .iter()
            .all(|segment| matches!(segment, Segment::Name(_)))
    }

    /// Returns the path the selector names, if it names just one.
    pub fn as_path(&self) -> Option<Path> {
        let mut path = Path::root();
        for segment in &self.segments {
            match segment {
                Segment::Name(name) => path = path.child(name),
                Segment::Pattern(_) | Segment::Any => return None,
            }
        }
        Some(path)
    }
}

impl Segment {
    /// Returns how narrowly the segment picks a name.
    ///
    /// The first number sorts the three kinds and the second breaks
    /// a tie between two patterns by how much each spells out.
    fn specificity(&self) -> (u8, usize) {
        match self {
            Self::Name(_) => (2, 0),
            Self::Pattern(pattern) => (1, pattern.len() - pattern.matches(ANY).count()),
            Self::Any => (0, 0),
        }
    }
}

/// Reports whether a pattern of literals and `*` spells a name.
///
/// Each `*` stands for any run of characters, including none. The
/// literal runs between them have to appear in order, so the walk
/// takes the earliest place each one fits, which never costs a
/// match a later choice would have found.
fn matches_pattern(pattern: &str, name: &str) -> bool {
    let mut runs = pattern.split(ANY);
    let opening = runs.next().expect("a split yields at least one run");
    let Some(mut rest) = name.strip_prefix(opening) else {
        return false;
    };

    let runs: Vec<&str> = runs.collect();
    let Some((closing, between)) = runs.split_last() else {
        // The pattern held no wildcard, so it is a whole name.
        return rest.is_empty();
    };

    for run in between {
        match rest.find(run) {
            Some(at) => rest = &rest[at + run.len()..],
            None => return false,
        }
    }

    // What follows the last wildcard has to close the name, and
    // cannot reuse the characters the runs before it took.
    rest.len() >= closing.len() && rest.ends_with(closing)
}

impl fmt::Display for Selector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut path = Path::root();
        for segment in &self.segments {
            path = match segment {
                Segment::Name(name) | Segment::Pattern(name) => path.child(name),
                Segment::Any => path.child(ANY),
            };
        }
        write!(f, "{path}")
    }
}

impl FromStr for Selector {
    type Err = PathError;

    fn from_str(pointer: &str) -> Result<Self, Self::Err> {
        let path: Path = pointer.parse()?;
        Ok(Self::from(&path))
    }
}

/// Reads a path as a selector, taking `*` as the wildcard.
///
/// A path that came from a URL or a config file carries the wildcard
/// as an ordinary segment, so the two forms convert here rather than
/// in every caller.
impl From<&Path> for Selector {
    fn from(path: &Path) -> Self {
        Self {
            segments: path
                .segments()
                .iter()
                .map(|name| {
                    if name == ANY {
                        Segment::Any
                    } else if name.contains(ANY) {
                        Segment::Pattern(name.clone())
                    } else {
                        Segment::Name(name.clone())
                    }
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use test_case::test_case;

    use super::*;

    /// Parses a selector that must be valid.
    fn selector(pointer: &str) -> Selector {
        pointer.parse().expect("valid pointer")
    }

    /// Parses a path that must be valid.
    fn path(pointer: &str) -> Path {
        pointer.parse().expect("valid pointer")
    }

    #[test]
    fn a_wildcard_matches_one_segment_of_any_name() {
        let every_board = selector("/boards/*/regulators/core/target_voltage_v");

        assert!(every_board.matches(&path(
            "/boards/bitaxe-gamma-e2f5/regulators/core/target_voltage_v"
        )));
        assert!(every_board.matches(&path(
            "/boards/emberone-00-abcd/regulators/core/target_voltage_v"
        )));
        assert!(!every_board.matches(&path("/boards/bitaxe-gamma-e2f5/fans/0/target_voltage_v")));
    }

    #[test]
    fn a_wildcard_does_not_span_segments() {
        let threads = selector("/boards/*/threads");

        assert!(!threads.matches(&path("/boards/bitaxe-gamma-e2f5/threads/0")));
        assert!(threads.matches(&path("/boards/bitaxe-gamma-e2f5/threads")));
    }

    #[test]
    fn a_wildcard_beside_other_text_matches_inside_one_segment() {
        let every_emberone = selector("/boards/emberone-00-*/threads/*/target_frequency_hz");

        assert!(every_emberone.matches(&path(
            "/boards/emberone-00-abcd/threads/0/target_frequency_hz"
        )));
        assert!(
            every_emberone.matches(&path("/boards/emberone-00-/threads/0/target_frequency_hz"))
        );
        assert!(!every_emberone.matches(&path(
            "/boards/bitaxe-gamma-abcd/threads/0/target_frequency_hz"
        )));
        assert!(
            !every_emberone.matches(&path("/boards/emberone-00/threads/0/target_frequency_hz"))
        );
    }

    #[test_case("emberone-00-*", "emberone-00-abcd", true; "opening run")]
    #[test_case("*-abcd", "emberone-00-abcd", true; "closing run")]
    #[test_case("*", "anything", true; "on its own")]
    #[test_case("*", "", true; "an empty name")]
    #[test_case("emberone*abcd", "emberone-00-abcd", true; "a run at each end")]
    #[test_case("emberone*00*abcd", "emberone-00-abcd", true; "a run in the middle")]
    #[test_case("emberone*00*abcd", "emberone-abcd", false; "a middle run that is missing")]
    #[test_case("a*a", "a", false; "runs that would have to share a character")]
    #[test_case("*ab*ab", "abab", true; "repeated runs")]
    #[test_case("*ab*ab", "aba", false; "a closing run cut short")]
    // A wildcard runs over the unescaped name, so a slash in a name
    // is an ordinary character to it, not a segment boundary.
    #[test_case("emberone-00-*", "emberone-00-a/b", true; "a name holding a slash")]
    fn a_pattern_spells_the_names_it_matches(pattern: &str, name: &str, matched: bool) {
        let segment = selector(&format!("/{}", pattern.replace('/', "~1")));

        assert_eq!(segment.matches(&Path::root().child(name)), matched);
    }

    #[test]
    fn a_pattern_stays_inside_its_own_segment() {
        let boards = selector("/boards/emberone*");

        assert!(boards.matches(&path("/boards/emberone-00-abcd")));
        assert!(!boards.matches(&path("/boards/emberone-00-abcd/threads")));
    }

    #[test]
    fn spelling_out_more_of_a_name_is_more_specific() {
        let model = selector("/boards/emberone-00-*/target_voltage_v");
        let vendor = selector("/boards/ember*/target_voltage_v");
        let every_board = selector("/boards/*/target_voltage_v");
        let one_board = selector("/boards/emberone-00-abcd/target_voltage_v");

        assert!(one_board.more_specific_than(&model));
        assert!(model.more_specific_than(&vendor));
        assert!(vendor.more_specific_than(&every_board));
        assert!(!every_board.more_specific_than(&vendor));
        assert!(!model.more_specific_than(&model));
    }

    #[test]
    fn a_pattern_names_a_class_rather_than_one_path() {
        let model = selector("/boards/emberone-00-*/target_voltage_v");

        assert!(!model.is_exact());
        assert_eq!(model.as_path(), None);
    }

    #[test]
    fn the_leftmost_concrete_segment_decides_specificity() {
        let instance = selector("/boards/bitaxe-gamma-e2f5/regulators/target_voltage_v");
        let class = selector("/boards/*/regulators/target_voltage_v");
        let every_model = selector("/boards/*/*/target_voltage_v");

        assert!(instance.more_specific_than(&class));
        assert!(!class.more_specific_than(&instance));
        assert!(class.more_specific_than(&every_model));
        assert!(!class.more_specific_than(&class));
    }

    #[test]
    fn a_selector_without_a_wildcard_names_one_path() {
        let one = selector("/boards/bitaxe-gamma-e2f5/target_voltage_v");
        let many = selector("/boards/*/target_voltage_v");

        assert!(one.is_exact());
        assert_eq!(
            one.as_path(),
            Some(path("/boards/bitaxe-gamma-e2f5/target_voltage_v"))
        );
        assert!(!many.is_exact());
        assert_eq!(many.as_path(), None);
    }

    #[test]
    fn a_selector_writes_back_the_pointer_it_was_read_from() {
        let pointer = "/boards/*/regulators/core/target_voltage_v";

        assert_eq!(selector(pointer).to_string(), pointer);
    }

    #[test]
    fn an_exact_selector_matches_only_its_own_path() {
        let one = Selector::exactly(&path("/boards/x/target_voltage_v"));

        assert!(one.matches(&path("/boards/x/target_voltage_v")));
        assert!(!one.matches(&path("/boards/y/target_voltage_v")));
    }
}
