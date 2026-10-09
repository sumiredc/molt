//! Checks content before it is written: which syntax elements and crates it
//! uses that the profile does not allow (PRD §3, §8.1).
//!
//! A pure function over strings: no file, no environment variable, no network.

mod crates;
mod diff;
pub mod finding;
mod macro_args;
pub mod visitor;

use std::ffi::OsStr;
use std::path::Path;

use crate::profile::status::{ItemKey, Status};

pub use finding::{CheckContext, CheckReport, Finding, ParseError};

/// Checks `new`, the content about to be written to `path`, against
/// `ctx.profile`.
///
/// An element or crate is reported unless it is a base element or `known`.
/// Content that does not parse (as Rust, or as TOML for a `Cargo.toml`) is
/// `ParseError`, whatever else it contains and whichever lines changed.
///
/// A `Cargo.toml` is checked for the dependency keys it gains over `old`
/// (`crates::in_manifest`); that comparison of keys is its whole filter.
///
/// Any other file is checked as Rust, for its elements and for the crates its
/// paths start from (`crates::in_source`). A crate is reported there even
/// when `Cargo.toml` already depends on it. An item is reported only when it
/// starts on a line changed from `old` (`old` is empty for a new file, and
/// then the whole of `new` is checked), so a crate is reported on each new
/// line that uses it, and not where existing code uses it. An item spanning
/// several lines is judged by `Finding.line` alone, so that editing one line
/// inside an existing `impl` does not report the whole `impl`; the detectors
/// therefore put `Finding.line` on the token that makes the item (the macro
/// path, the `?`, the method name, the first segment of a path), not on the
/// start of an enclosing expression.
///
/// The arguments of the std macros that take Rust expressions (`println!`,
/// `vec!`, `matches!` and the like) are checked like the code around them
/// (`macro_args`); such a macro whose arguments do not parse in its shape is
/// also a `macro_call`. The inside of any other macro is not seen, and its
/// call is itself a finding.
pub fn check_file(path: &Path, old: &str, new: &str, ctx: &CheckContext) -> CheckReport {
    let findings: Vec<Finding> = if path.file_name() == Some(OsStr::new("Cargo.toml")) {
        match crates::in_manifest(old, new) {
            Ok(detected) => detected,
            Err(error) => return CheckReport::ParseError(error),
        }
    } else {
        let mut detected = match visitor::detect(new) {
            Ok(detected) => detected,
            Err(error) => return CheckReport::ParseError(error),
        };
        detected.extend(crates::in_source(new, ctx.own_modules));
        detected.sort_by_key(|finding| finding.line);
        let changed = diff::changed_lines(old, new);
        detected
            .into_iter()
            .filter(|finding| changed.contains(finding.line))
            .collect()
    };
    let findings: Vec<Finding> = findings
        .into_iter()
        .filter(|finding| !is_allowed(finding, ctx))
        .collect();
    if findings.is_empty() {
        CheckReport::Clean
    } else {
        CheckReport::OutOfProfile(findings)
    }
}

/// Whether the content may use the item: a base element, or `known`. A crate
/// is looked up under every spelling `crates::crate_key` takes to its key
/// (`crate:serde-json` for `serde_json`), and is `known` when one of them is
/// and none is anything else: a spelling the user made `seen` or `unknown`
/// still stops it.
fn is_allowed(finding: &Finding, ctx: &CheckContext) -> bool {
    match &finding.key {
        ItemKey::Element(element) => {
            ctx.profile.is_base(*element) || ctx.profile.status(&finding.key) == Status::Known
        }
        ItemKey::Crate(_) => {
            let mut statuses = ctx
                .profile
                .entries()
                .filter(|(key, _)| {
                    matches!(key, ItemKey::Crate(name) if crates::crate_key(name) == finding.key)
                })
                .map(|(_, entry)| entry.status())
                .peekable();
            statuses.peek().is_some() && statuses.all(|status| status == Status::Known)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::profile::element::Element;
    use crate::profile::status::Provenance;
    use crate::profile::store::{Profile, Timestamp};

    fn check(profile: &Profile, new: &str) -> CheckReport {
        let own_modules = BTreeSet::new();
        let ctx = CheckContext {
            profile,
            own_modules: &own_modules,
        };
        check_file(Path::new("src/lib.rs"), "", new, &ctx)
    }

    fn with_status(element: Element, status: Status) -> Profile {
        let mut profile = Profile::new();
        profile
            .transition(
                ItemKey::Element(element),
                status,
                Provenance::Manual,
                Timestamp::from_unix_seconds(0),
                None,
            )
            .unwrap();
        profile
    }

    fn reported(report: &CheckReport) -> Vec<(usize, ItemKey)> {
        match report {
            CheckReport::OutOfProfile(findings) => {
                findings.iter().map(|f| (f.line, f.key.clone())).collect()
            }
            other => panic!("expected OutOfProfile, got {other:?}"),
        }
    }

    #[test]
    fn content_with_only_base_elements_is_clean() {
        let report = check(&Profile::new(), "fn main() {\n    println!(\"hi\");\n}\n");
        assert_eq!(report, CheckReport::Clean);
    }

    #[test]
    fn an_element_that_is_neither_base_nor_known_is_reported_with_its_line() {
        let report = check(&Profile::new(), "fn main() {\n    dbg!(1);\n}\n");
        assert_eq!(
            reported(&report),
            [(2, ItemKey::Element(Element::DbgMacro))]
        );
    }

    #[test]
    fn a_known_element_is_not_reported() {
        let profile = with_status(Element::DbgMacro, Status::Known);
        let report = check(&profile, "fn main() {\n    dbg!(1);\n}\n");
        assert_eq!(report, CheckReport::Clean);
    }

    #[test]
    fn a_seen_element_is_reported() {
        let profile = with_status(Element::DbgMacro, Status::Seen);
        let report = check(&profile, "fn main() {\n    dbg!(1);\n}\n");
        assert_eq!(
            reported(&report),
            [(2, ItemKey::Element(Element::DbgMacro))]
        );
    }

    #[test]
    fn a_base_element_in_deny_is_reported() {
        let profile = with_status(Element::PrintMacro, Status::Unknown);
        assert!(profile.deny().contains(&Element::PrintMacro));
        let report = check(&profile, "fn main() {\n    println!(\"hi\");\n}\n");
        assert_eq!(
            reported(&report),
            [(2, ItemKey::Element(Element::PrintMacro))]
        );
    }

    #[test]
    fn content_that_does_not_parse_is_a_parse_error_even_with_findings_before_it() {
        let report = check(
            &Profile::new(),
            "fn main() {\n    dbg!(1);\n    let = ;\n}\n",
        );
        match report {
            CheckReport::ParseError(error) => assert_eq!(error.line, 3),
            other => panic!("expected ParseError, got {other:?}"),
        }
    }
}
