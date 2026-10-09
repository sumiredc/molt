//! The checker against `tests/fixtures/elements/*.rs` (PRD §9).
//!
//! Each fixture starts with `// expect: a, b`, the names of the elements in
//! it, and is named after the element it is for (`print_macro.rs`). The set
//! the checker detects must equal the expected set exactly. Every element
//! has its fixture.
//!
//! `tests/fixtures/negative/*.rs` hold code with base elements only, and the
//! checker must find nothing else in them.
//!
//! The tests through `check_file` hold the visitor to the line it reports an
//! element on: the line of the token that makes it, which the changed-line
//! filter looks at.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use molt::checker::{CheckContext, CheckReport, check_file, visitor};
use molt::profile::baseline;
use molt::profile::element::Element;
use molt::profile::status::{ItemKey, Provenance, Status};
use molt::profile::store::{Profile, Timestamp};

const EXPECT: &str = "// expect:";

fn elements_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/elements")
}

fn negative_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/negative")
}

/// Every `*.rs` in `dir`, sorted by name.
fn fixtures(dir: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
        .collect();
    paths.sort();
    paths
}

/// The elements listed in the first line, `// expect: a, b`. An empty list
/// means none.
fn expected(source: &str) -> Result<BTreeSet<Element>, String> {
    let first = source.lines().next().unwrap_or("");
    let list = first
        .strip_prefix(EXPECT)
        .ok_or_else(|| format!("the first line is not `{EXPECT} ..`: {first:?}"))?;
    list.split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|name| name.parse::<Element>().map_err(|e| e.to_string()))
        .collect()
}

fn detected(source: &str) -> Result<BTreeSet<Element>, String> {
    let findings = visitor::detect(source).map_err(|e| format!("parse error at {e}"))?;
    findings
        .into_iter()
        .map(|finding| match finding.key {
            ItemKey::Element(element) => Ok(element),
            ItemKey::Crate(name) => Err(format!("unexpected crate {name}")),
        })
        .collect()
}

fn names(elements: &BTreeSet<Element>) -> Vec<&'static str> {
    elements.iter().map(|e| e.name()).collect()
}

#[test]
fn every_element_fixture_detects_exactly_the_expected_elements() {
    let paths = fixtures(&elements_dir());
    assert!(
        !paths.is_empty(),
        "no fixture in {}",
        elements_dir().display()
    );

    let mut failures = Vec::new();
    for path in &paths {
        let source = fs::read_to_string(path).unwrap();
        let result = expected(&source).and_then(|expected| {
            let detected = detected(&source)?;
            if detected == expected {
                Ok(())
            } else {
                Err(format!(
                    "expected {:?}, detected {:?}",
                    names(&expected),
                    names(&detected)
                ))
            }
        });
        if let Err(message) = result {
            failures.push(format!("{}: {message}", path.display()));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

#[test]
fn every_element_fixture_is_named_after_an_element_it_expects() {
    for path in fixtures(&elements_dir()) {
        let stem = path.file_stem().unwrap().to_str().unwrap();
        let element: Element = stem
            .parse()
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let source = fs::read_to_string(&path).unwrap();
        assert!(
            expected(&source).unwrap().contains(&element),
            "{}: does not expect {element}",
            path.display()
        );
    }
}

#[test]
fn every_element_has_a_fixture_named_after_it() {
    let names: BTreeSet<String> = fixtures(&elements_dir())
        .iter()
        .map(|path| path.file_stem().unwrap().to_str().unwrap().to_owned())
        .collect();
    let missing: Vec<&str> = Element::ALL
        .iter()
        .map(|e| e.name())
        .filter(|name| !names.contains(*name))
        .collect();
    assert!(missing.is_empty(), "no fixture for {missing:?}");
}

#[test]
fn negative_fixtures_contain_no_element_other_than_base_elements() {
    let paths = fixtures(&negative_dir());
    assert!(
        !paths.is_empty(),
        "no fixture in {}",
        negative_dir().display()
    );

    let mut failures = Vec::new();
    for path in &paths {
        let source = fs::read_to_string(path).unwrap();
        match detected(&source) {
            Ok(detected) => {
                let other: BTreeSet<Element> = detected
                    .into_iter()
                    .filter(|e| !baseline::DEFAULT.contains(e))
                    .collect();
                if !other.is_empty() {
                    failures.push(format!("{}: {:?}", path.display(), names(&other)));
                }
            }
            Err(message) => failures.push(format!("{}: {message}", path.display())),
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

#[test]
fn a_question_mark_added_to_a_later_line_of_an_existing_chain_is_reported_on_that_line() {
    // `?` is a base element; deny it so that the check reports it.
    let mut profile = Profile::new();
    profile
        .transition(
            ItemKey::Element(Element::QuestionMark),
            Status::Unknown,
            Provenance::Manual,
            Timestamp::now(),
            None,
        )
        .unwrap();
    let own_modules = BTreeSet::new();
    let ctx = CheckContext {
        profile: &profile,
        own_modules: &own_modules,
    };
    let old = "fn f(s: S) -> R {\n    let n = s\n        .first()\n        .second();\n}\n";
    let new = "fn f(s: S) -> R {\n    let n = s\n        .first()?\n        .second();\n}\n";

    let report = check_file(Path::new("src/lib.rs"), old, new, &ctx);

    let CheckReport::OutOfProfile(findings) = report else {
        panic!("expected OutOfProfile, got {report:?}");
    };
    let reported: Vec<(usize, ItemKey)> = findings.into_iter().map(|f| (f.line, f.key)).collect();
    assert_eq!(reported, [(3, ItemKey::Element(Element::QuestionMark))]);
}

#[test]
fn a_name_imported_on_an_unchanged_line_and_used_on_a_new_line_is_reported_on_the_new_line() {
    let profile = Profile::new();
    let own_modules = BTreeSet::new();
    let ctx = CheckContext {
        profile: &profile,
        own_modules: &own_modules,
    };
    let old = "use std::mem::take;\n\nfn f(v: Vec<u8>) {\n}\n";
    let new = "use std::mem::take;\n\nfn f(v: Vec<u8>) {\n    take(v);\n}\n";

    let report = check_file(Path::new("src/lib.rs"), old, new, &ctx);

    let CheckReport::OutOfProfile(findings) = report else {
        panic!("expected OutOfProfile, got {report:?}");
    };
    let reported: Vec<(usize, ItemKey)> = findings.into_iter().map(|f| (f.line, f.key)).collect();
    assert_eq!(reported, [(4, ItemKey::Element(Element::MemTake))]);
}

#[test]
fn the_expect_line_lists_comma_separated_element_names() {
    assert_eq!(
        expected("// expect: arc, mutex\nfn f() {}\n"),
        Ok(BTreeSet::from([Element::Arc, Element::Mutex]))
    );
}

#[test]
fn an_empty_expect_line_expects_no_element() {
    assert_eq!(expected("// expect:\nfn f() {}\n"), Ok(BTreeSet::new()));
}

#[test]
fn a_fixture_without_the_expect_line_is_rejected() {
    assert!(expected("fn f() {}\n// expect: arc\n").is_err());
}

#[test]
fn an_expect_line_with_a_name_that_is_not_an_element_is_rejected() {
    assert!(expected("// expect: arc, not_an_element\n").is_err());
}
