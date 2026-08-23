//! Paths into the systree, written as RFC 6901 JSON Pointers.

use std::fmt;
use std::str::FromStr;

use thiserror::Error;

/// The path of one node in the systree.
///
/// Holds the path as unescaped segments rather than as one string,
/// so that two paths compare one segment at a time. A scan of the
/// subtree under `/boards/x` then takes every path beginning with
/// those two segments and no others. Comparing paths as strings
/// would put `/boards/x-2` inside that subtree.
///
/// The external form is an RFC 6901 JSON Pointer, read by [`FromStr`]
/// and written by [`fmt::Display`].
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Path {
    segments: Vec<String>,
}

/// A JSON Pointer that names no path.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PathError {
    #[error("Path must start with '/'")]
    MissingLeadingSlash,

    #[error("Invalid escape in path segment: {0}")]
    InvalidEscape(String),
}

impl Path {
    /// Returns the path of the root, which is the miner itself.
    pub fn root() -> Self {
        Self::default()
    }

    /// Returns the path of the named child.
    ///
    /// The name is a literal segment. One holding `/` or `~` needs no
    /// escaping here, only in the JSON Pointer form.
    ///
    /// # Example
    ///
    /// ```
    /// use mujina_miner::systree::Path;
    ///
    /// let boards = Path::root().child("boards");
    /// let board = boards.child("bitaxe-e2f56f9b");
    /// assert_eq!(board.to_string(), "/boards/bitaxe-e2f56f9b");
    ///
    /// let slashed = Path::root().child("a/b");
    /// assert_eq!(slashed.segments(), ["a/b"]);
    /// assert_eq!(slashed.to_string(), "/a~1b");
    /// ```
    pub fn child(&self, name: &str) -> Self {
        let mut segments = self.segments.clone();
        segments.push(name.to_string());
        Self { segments }
    }

    /// Returns the unescaped segments, ordered from the root down to
    /// this node.
    pub fn segments(&self) -> &[String] {
        &self.segments
    }

    /// Reports whether this path lies at or beneath `prefix`.
    pub fn starts_with(&self, prefix: &Path) -> bool {
        self.segments.starts_with(&prefix.segments)
    }
}

impl fmt::Display for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for segment in &self.segments {
            f.write_str("/")?;
            for c in segment.chars() {
                match c {
                    '~' => f.write_str("~0")?,
                    '/' => f.write_str("~1")?,
                    _ => write!(f, "{c}")?,
                }
            }
        }
        Ok(())
    }
}

impl FromStr for Path {
    type Err = PathError;

    fn from_str(pointer: &str) -> Result<Self, Self::Err> {
        if pointer.is_empty() {
            return Ok(Self::root());
        }
        let Some(body) = pointer.strip_prefix('/') else {
            return Err(PathError::MissingLeadingSlash);
        };
        let segments = body
            .split('/')
            .map(unescape)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { segments })
    }
}

/// Turns one escaped reference token into its literal segment.
fn unescape(token: &str) -> Result<String, PathError> {
    let mut segment = String::with_capacity(token.len());
    let mut chars = token.chars();
    while let Some(c) = chars.next() {
        if c != '~' {
            segment.push(c);
            continue;
        }
        // RFC 6901 defines these two escapes and no others, so a
        // stray tilde is an error rather than a literal.
        match chars.next() {
            Some('0') => segment.push('~'),
            Some('1') => segment.push('/'),
            _ => return Err(PathError::InvalidEscape(token.to_string())),
        }
    }
    Ok(segment)
}

#[cfg(test)]
mod tests {
    use test_case::test_case;

    use super::*;

    /// Parses a pointer that must be valid.
    fn path(pointer: &str) -> Path {
        pointer.parse().expect("valid pointer")
    }

    #[test]
    fn child_extends_the_path() {
        let board = Path::root().child("boards").child("bitaxe-e2f56f9b");
        let hashes = board.child("threads").child("0").child("hashes_per_s");

        assert_eq!(board.to_string(), "/boards/bitaxe-e2f56f9b");
        assert_eq!(
            hashes.to_string(),
            "/boards/bitaxe-e2f56f9b/threads/0/hashes_per_s"
        );
    }

    // The tilde rows are the ones that break a two-pass encoder or
    // decoder. Decoding ~0 first turns /~01 into a slash on the
    // second pass, and escaping the slash first turns a/b into
    // a~01b.
    #[test_case("", &[]; "root")]
    #[test_case("/boards", &["boards"]; "one_name")]
    #[test_case(
        "/boards/bitaxe-e2f56f9b/threads/0/hashes_per_s",
        &["boards", "bitaxe-e2f56f9b", "threads", "0", "hashes_per_s"];
        "nested"
    )]
    #[test_case("/", &[""]; "empty_name")]
    #[test_case("/~0", &["~"]; "lone_tilde")]
    #[test_case("/a~0", &["a~"]; "trailing_tilde")]
    #[test_case("/~00", &["~0"]; "literal_tilde_escape")]
    #[test_case("/~01", &["~1"]; "literal_slash_escape")]
    #[test_case("/~001", &["~01"]; "literal_tilde_escape_before_a_one")]
    #[test_case("/a~1b", &["a/b"]; "slash_in_name")]
    #[test_case("/a~1b~0c", &["a/b~c"]; "both_escapes")]
    #[test_case("/~1~1", &["//"]; "only_slashes")]
    #[test_case("/caf\u{e9}", &["caf\u{e9}"]; "outside_ascii")]
    fn converts_between_a_pointer_and_its_segments(pointer: &str, segments: &[&str]) {
        let parsed = path(pointer);

        assert_eq!(parsed.segments(), segments);
        assert_eq!(parsed.to_string(), pointer);
    }

    #[test]
    fn root_is_the_empty_pointer() {
        assert_eq!(Path::root().to_string(), "");
        assert_eq!(path(""), Path::root());
        assert!(Path::root().segments().is_empty());
    }

    #[test]
    fn rejects_a_pointer_without_a_leading_slash() {
        assert_eq!(
            "boards/x".parse::<Path>(),
            Err(PathError::MissingLeadingSlash)
        );
    }

    #[test]
    fn rejects_an_unknown_escape() {
        assert!(matches!(
            "/a~2".parse::<Path>(),
            Err(PathError::InvalidEscape(_))
        ));
        assert!(matches!(
            "/a~".parse::<Path>(),
            Err(PathError::InvalidEscape(_))
        ));
    }

    #[test]
    fn a_sibling_sharing_a_prefix_string_lies_outside_the_subtree() {
        let board = path("/boards/x");

        assert!(!path("/boards/x-2").starts_with(&board));
        assert!(path("/boards/x/threads/0").starts_with(&board));
        assert!(board.starts_with(&board));
        assert!(board.starts_with(&Path::root()));
    }
}
