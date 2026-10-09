//! Checks content before it is written: which syntax elements and crates it
//! uses that the profile does not allow (PRD §3, §8.1).
//!
//! A pure function over strings: no file, no environment variable, no network.

pub mod finding;
pub mod visitor;

use std::path::Path;

use crate::profile::status::{ItemKey, Status};

pub use finding::{CheckContext, CheckReport, Finding, ParseError};

/// Checks `new`, the content about to be written to `path`, against
/// `ctx.profile`.
///
/// An element is reported unless it is a base element or `known`. Content
/// that syn cannot parse is `ParseError`, whatever else it contains.
///
/// For now the whole of `new` is checked, and `path`, `old` and
/// `ctx.own_modules` are not used yet.
// TODO(#36): report only the lines that differ from `old`.
// TODO(#37): detect crates from the paths in `new` and, when `path` is a
// `Cargo.toml`, from its dependency keys.
pub fn check_file(_path: &Path, _old: &str, new: &str, ctx: &CheckContext) -> CheckReport {
    let detected = match visitor::detect(new) {
        Ok(detected) => detected,
        Err(error) => return CheckReport::ParseError(error),
    };
    let findings: Vec<Finding> = detected
        .into_iter()
        .filter(|finding| !is_allowed(finding, ctx))
        .collect();
    if findings.is_empty() {
        CheckReport::Clean
    } else {
        CheckReport::OutOfProfile(findings)
    }
}

/// Whether the content may use the item: a base element, or `known`.
fn is_allowed(finding: &Finding, ctx: &CheckContext) -> bool {
    let base = match finding.key {
        ItemKey::Element(element) => ctx.profile.is_base(element),
        ItemKey::Crate(_) => false,
    };
    base || ctx.profile.status(&finding.key) == Status::Known
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
