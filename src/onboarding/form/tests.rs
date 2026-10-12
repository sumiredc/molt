use std::collections::{BTreeMap, BTreeSet};

use super::*;
use crate::profile::store::Timestamp;
use crate::repo_scan::Place;
use crate::ui::{ITEMS, TabValues};

fn locale<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
    move |name| {
        vars.iter()
            .find(|(n, _)| *n == name)
            .map(|(_, value)| (*value).to_owned())
    }
}

#[test]
fn japanese_only_when_the_first_non_empty_locale_starts_with_ja() {
    let cases: [(&[(&str, &str)], Language); 7] = [
        (&[("LANG", "ja_JP.UTF-8")], Language::Japanese),
        (
            &[("LC_MESSAGES", "ja_JP.UTF-8"), ("LANG", "en_US.UTF-8")],
            Language::Japanese,
        ),
        (
            &[("LC_ALL", "en_US.UTF-8"), ("LANG", "ja_JP.UTF-8")],
            Language::English,
        ),
        (
            &[("LC_ALL", ""), ("LANG", "ja_JP.UTF-8")],
            Language::Japanese,
        ),
        (
            &[("LC_ALL", ""), ("LC_MESSAGES", ""), ("LANG", "")],
            Language::English,
        ),
        (&[("LANG", "C")], Language::English),
        (&[], Language::English),
    ];
    for (vars, language) in cases {
        assert_eq!(Language::from_locale(locale(vars)), language, "{vars:?}");
    }
}

#[test]
fn every_category_has_a_title_and_a_question_with_the_checked_names_in_both_languages() {
    let mut seen = BTreeSet::new();
    for (category, language, title, question) in templates() {
        assert!(!title.is_empty(), "{category} {language:?}");
        assert_eq!(
            question.matches(ITEMS).count(),
            1,
            "{category} {language:?}"
        );
        seen.insert((category.name(), language == Language::Japanese));
    }
    assert_eq!(seen.len(), Category::ALL.len() * 2);
}

#[test]
fn the_form_has_a_tab_per_category_with_its_elements_and_the_three_reports() {
    for language in [Language::English, Language::Japanese] {
        let form = build(language, None);
        assert_eq!(form.tabs.len(), Category::ALL.len());
        for (tab, category) in form.tabs.iter().zip(Category::ALL) {
            let names: Vec<&str> = category.elements().map(Element::name).collect();
            assert_eq!(tab.items, names, "{category}");
            assert_eq!(tab.choices.len(), 3);
            assert_eq!(tab.checklist_on, 0, "the first report shows the checklist");
            for name in &names {
                assert!(tab.prompt.contains(name), "{}", tab.prompt);
            }
        }
    }
}

#[test]
fn the_strings_are_english_or_japanese_by_the_language() {
    let english = build(Language::English, None);
    assert_eq!(english.tabs[0].title, "ownership");
    assert_eq!(
        english.tabs[0].choices,
        ["have written", "can read", "don't know"]
    );
    assert_eq!(english.text.submit_title, "Submit");

    let japanese = build(Language::Japanese, None);
    assert_eq!(japanese.tabs[0].title, "所有権");
    assert_eq!(
        japanese.tabs[0].choices,
        ["書いたことがある", "読める", "知らない"]
    );
    assert_eq!(japanese.text.submit_title, "送信");
    assert!(
        japanese.tabs[0]
            .question
            .starts_with("{items} をどこで使いましたか")
    );
}

#[test]
fn the_places_the_reference_repository_uses_an_element_are_its_material() {
    let scan = RepoScan {
        repo: "/src/app".to_owned(),
        at: Timestamp::from_unix_seconds(0),
        crates: Vec::new(),
        skipped: Vec::new(),
        elements: BTreeMap::from([(
            "clone".to_owned(),
            vec![
                Place {
                    path: "src/a.rs".to_owned(),
                    line: 3,
                },
                Place {
                    path: "src/b.rs".to_owned(),
                    line: 9,
                },
            ],
        )]),
    };
    let form = build(Language::English, Some(&scan));
    let ownership = &form.tabs[0];
    let clone = ownership
        .items
        .iter()
        .position(|item| item == "clone")
        .unwrap();
    assert_eq!(
        ownership.material[clone],
        ["`clone` in your repository: src/a.rs:3, src/b.rs:9"]
    );
    let others: Vec<_> = ownership
        .material
        .iter()
        .enumerate()
        .filter(|(at, _)| *at != clone)
        .collect();
    assert!(
        others.iter().all(|(_, lines)| lines.is_empty()),
        "{others:?}"
    );
    assert!(
        build(Language::English, None).tabs[0]
            .material
            .iter()
            .all(Vec::is_empty)
    );
}

/// Values with `ownership` as `tab` and every other category "don't know".
fn values(form: &Form, ownership: TabValues) -> FormValues {
    let mut values = form.empty_values();
    for tab in &mut values.tabs {
        tab.choice = Some(2);
    }
    values.tabs[0] = ownership;
    values
}

#[test]
fn a_have_written_category_with_checks_and_an_answer_is_sent_with_the_question_as_seen() {
    let form = build(Language::English, None);
    let answers = read(
        &form,
        &values(
            &form,
            TabValues {
                choice: Some(0),
                checked: BTreeSet::from([0, 1]),
                answer: "I clone strings.".to_owned(),
            },
        ),
    );
    let ownership = &answers[0];
    assert_eq!(ownership.report, Report::HaveWritten);
    assert_eq!(ownership.checked, [Element::Clone, Element::ToOwned]);
    assert!(
        ownership
            .question
            .starts_with("Where have you used clone, to_owned?")
    );
    assert!(ownership.sent());
    assert!(
        answers[1..]
            .iter()
            .all(|answer| !answer.sent() && answer.report == Report::DontKnow)
    );
}

#[test]
fn a_blank_answer_or_no_checks_is_not_sent() {
    let form = build(Language::English, None);
    for ownership in [
        TabValues {
            choice: Some(0),
            checked: BTreeSet::from([0]),
            answer: " \t".to_owned(),
        },
        TabValues {
            choice: Some(0),
            checked: BTreeSet::new(),
            answer: "kept from before".to_owned(),
        },
    ] {
        let answers = read(&form, &values(&form, ownership.clone()));
        assert!(!answers[0].sent(), "{ownership:?}");
        assert_eq!(answers[0].report, Report::HaveWritten);
    }
}

#[test]
fn checks_and_an_answer_kept_under_another_report_are_not_the_category_s() {
    let form = build(Language::English, None);
    let answers = read(
        &form,
        &values(
            &form,
            TabValues {
                choice: Some(1),
                checked: BTreeSet::from([0]),
                answer: "kept".to_owned(),
            },
        ),
    );
    assert_eq!(answers[0].report, Report::CanRead);
    assert!(answers[0].checked.is_empty());
    assert!(answers[0].answer.is_empty());
    assert!(!answers[0].sent());
}
