//! Browse-path pattern matching (glob + regex) with ancestor awareness.
//!
//! Globs use *separator-crossing* semantics (`*` matches across `/`) which makes
//! intuitive patterns like `*/Motor/*` match `Factory/Line1/Motor/Speed`.

use globset::{Glob, GlobSet, GlobSetBuilder};
use regex::Regex;

use crate::error::{Result, ScanError};

/// A compiled set of glob and regex patterns over browse paths.
#[derive(Debug, Default)]
pub struct PatternSet {
    globs: Option<GlobSet>,
    regexes: Vec<Regex>,
    /// Longest literal (wildcard-free) prefix of each glob, for descend pruning.
    literal_prefixes: Vec<String>,
    /// True when this set contains a regex (regex prefixes can't be pruned safely).
    has_regex: bool,
    empty: bool,
}

impl PatternSet {
    /// Compile a pattern set from glob and regex strings.
    pub fn compile(globs: &[String], regexes: &[String]) -> Result<Self> {
        let empty = globs.is_empty() && regexes.is_empty();

        let mut builder = GlobSetBuilder::new();
        let mut literal_prefixes = Vec::new();
        for g in globs {
            let glob = Glob::new(g).map_err(|e| ScanError::Pattern {
                pattern: g.clone(),
                reason: e.to_string(),
            })?;
            builder.add(
                Glob::new(g)
                    .map(|_| glob.clone())
                    .map_err(|e| ScanError::Pattern {
                        pattern: g.clone(),
                        reason: e.to_string(),
                    })?,
            );
            literal_prefixes.push(literal_prefix(g));
        }
        let globset = if globs.is_empty() {
            None
        } else {
            Some(builder.build().map_err(|e| ScanError::Pattern {
                pattern: globs.join(", "),
                reason: e.to_string(),
            })?)
        };

        let mut compiled_regexes = Vec::new();
        for r in regexes {
            let re = Regex::new(r).map_err(|e| ScanError::Pattern {
                pattern: r.clone(),
                reason: e.to_string(),
            })?;
            compiled_regexes.push(re);
        }

        Ok(Self {
            globs: globset,
            has_regex: !compiled_regexes.is_empty(),
            regexes: compiled_regexes,
            literal_prefixes,
            empty,
        })
    }

    /// Whether this set holds no patterns.
    pub fn is_empty(&self) -> bool {
        self.empty
    }

    /// Whether `path` matches any pattern in the set.
    pub fn matches(&self, path: &str) -> bool {
        if self.empty {
            return false;
        }
        if let Some(globs) = &self.globs
            && globs.is_match(path)
        {
            return true;
        }
        self.regexes.iter().any(|re| re.is_match(path))
    }

    /// Whether `path` could be an ancestor (prefix) of a future match, used to
    /// decide whether to descend into a branch. Conservative: returns `true`
    /// whenever it cannot prove the branch is hopeless (e.g. for regex patterns).
    pub fn prefix_compatible(&self, path: &str) -> bool {
        if self.empty || self.has_regex {
            return true;
        }
        // A branch is worth descending if its path is prefix-compatible with at
        // least one glob's literal prefix.
        self.literal_prefixes
            .iter()
            .any(|lit| prefix_compatible(lit, path))
    }
}

/// Compute the leading wildcard-free portion of a glob pattern.
fn literal_prefix(glob: &str) -> String {
    let mut out = String::new();
    let mut chars = glob.chars().peekable();
    while let Some(&c) = chars.peek() {
        if matches!(c, '*' | '?' | '[' | '{' | '\\') {
            break;
        }
        out.push(c);
        chars.next();
    }
    out
}

/// Whether a literal prefix and a browse path are prefix-compatible at segment
/// boundaries (one is a prefix of the other, with the last literal segment
/// allowed to be partial).
fn prefix_compatible(literal: &str, path: &str) -> bool {
    if literal.is_empty() || path.is_empty() {
        return true;
    }
    let lit_segs: Vec<&str> = literal.split('/').collect();
    let path_segs: Vec<&str> = path.split('/').collect();
    let common = lit_segs.len().min(path_segs.len());

    for i in 0..common {
        let l = lit_segs[i];
        let p = path_segs[i];
        let is_last_lit = i + 1 == lit_segs.len();
        if is_last_lit {
            // The literal prefix may stop mid-segment.
            if !(p.starts_with(l) || l.starts_with(p)) {
                return false;
            }
        } else if l != p {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_crosses_separators() {
        let set = PatternSet::compile(&["*/Motor/*".into()], &[]).unwrap();
        assert!(set.matches("Factory/Line1/Motor/Speed"));
        assert!(!set.matches("Factory/Line1/Pump/Speed"));
    }

    #[test]
    fn prefix_allows_ancestor_descent() {
        let set = PatternSet::compile(&["Line1/*".into()], &[]).unwrap();
        // Root and the Line1 branch should be descendable.
        assert!(set.prefix_compatible(""));
        assert!(set.prefix_compatible("Line1"));
        assert!(set.prefix_compatible("Line1/Motor"));
        // A sibling branch should be prunable.
        assert!(!set.prefix_compatible("Line2"));
    }

    #[test]
    fn regex_matches_and_is_conservative_for_descent() {
        let set = PatternSet::compile(&[], &["^Line[0-9]+/Motor.*".into()]).unwrap();
        assert!(set.matches("Line12/Motor/Speed"));
        // Regex sets never prune descent.
        assert!(set.prefix_compatible("Anything"));
    }

    #[test]
    fn empty_set_matches_nothing_but_descends() {
        let set = PatternSet::compile(&[], &[]).unwrap();
        assert!(!set.matches("x"));
        assert!(set.prefix_compatible("x"));
    }

    #[test]
    fn literal_prefix_stops_at_first_wildcard() {
        assert_eq!(literal_prefix("Line1/Motor/*"), "Line1/Motor/");
        assert_eq!(literal_prefix("Line1/*/Speed"), "Line1/");
        assert_eq!(literal_prefix("*/Motor"), "");
        assert_eq!(literal_prefix("Plain/Path"), "Plain/Path");
        assert_eq!(literal_prefix("a?b"), "a");
        assert_eq!(literal_prefix("a[bc]"), "a");
        assert_eq!(literal_prefix("a{b,c}"), "a");
    }

    #[test]
    fn invalid_glob_and_regex_are_reported() {
        // Unbalanced bracket is an invalid glob.
        assert!(PatternSet::compile(&["a[".into()], &[]).is_err());
        // Unbalanced group is an invalid regex.
        assert!(PatternSet::compile(&[], &["(".into()]).is_err());
    }

    #[test]
    fn multiple_globs_are_ored_together() {
        let set = PatternSet::compile(&["**/Temperature".into(), "**/Speed".into()], &[]).unwrap();
        assert!(set.matches("Factory/Line1/Motor/Temperature"));
        assert!(set.matches("Factory/Line1/Motor/Speed"));
        assert!(!set.matches("Factory/Line1/Motor/Torque"));
    }

    #[test]
    fn prefix_compatible_handles_partial_last_segment() {
        let set = PatternSet::compile(&["Line1/Mot*".into()], &[]).unwrap();
        // "Mot" is a partial of "Motor" so the branch is descendable.
        assert!(set.prefix_compatible("Line1/Motor"));
        assert!(set.prefix_compatible("Line1"));
        assert!(!set.prefix_compatible("Line2"));
    }

    proptest::proptest! {
        /// Invariant: a concrete glob match must never be pruned during descent.
        /// If a full path matches a glob, every ancestor prefix of that path
        /// must be reported as `prefix_compatible` (otherwise traversal would
        /// stop before reaching the matching node).
        #[test]
        fn matches_implies_ancestors_are_descendable(
            seg in proptest::collection::vec("[A-Za-z][A-Za-z0-9]{0,5}", 1..6)
        ) {
            let path = seg.join("/");
            // A glob anchored on the full literal path.
            let set = PatternSet::compile(std::slice::from_ref(&path), &[]).unwrap();
            proptest::prop_assert!(set.matches(&path));
            // Every ancestor prefix must remain descendable.
            for i in 1..seg.len() {
                let ancestor = seg[..i].join("/");
                proptest::prop_assert!(
                    set.prefix_compatible(&ancestor),
                    "ancestor {ancestor:?} pruned for path {path:?}"
                );
            }
        }
    }
}
