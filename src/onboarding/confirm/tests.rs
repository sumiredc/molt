use std::fs;
use std::path::{Path, PathBuf};

use serde_json::json;
use tempfile::TempDir;

use super::*;
use crate::config::Config;
use crate::llm::types::ChatResponse;
use crate::llm::{ChatError, FakeChatClient, Router};
use crate::onboarding::form::Language;
use crate::onboarding::judge::Model;
use crate::onboarding::questionnaire::test_support::form;
use crate::onboarding::questionnaire::{Outcome, init};
use crate::profile::element::{Category, Element};
use crate::profile::status::{ItemKey, Provenance, Status};
use crate::profile::store::{Report, Timestamp};
use crate::ui::{ScriptedUi, Step};

/// The empty line that ends the confirmation and saves.
fn save() -> Step {
    Step::Input(String::new())
}

fn input(text: &str) -> Step {
    Step::Input(text.to_owned())
}

fn element(element: Element) -> ItemKey {
    ItemKey::Element(element)
}

fn at(seconds: u64) -> Timestamp {
    Timestamp::from_unix_seconds(seconds)
}

fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// A profile as init builds it before the confirmation: ownership reported "have written" with
/// `clone` judged known by the Q&A, and the crate `tokio` known from the reference repository.
fn judged_profile() -> Profile {
    let mut profile = Profile::new();
    profile.set_report(Category::Ownership, Report::HaveWritten);
    profile
        .transition(
            element(Element::Clone),
            Status::Known,
            Provenance::Qa,
            at(1_791_590_400),
            Some("clones strings".to_owned()),
        )
        .unwrap();
    profile
        .transition(
            ItemKey::Crate("tokio".to_owned()),
            Status::Known,
            Provenance::ReferenceRepo,
            at(1_791_590_400),
            Some("/src/app".to_owned()),
        )
        .unwrap();
    profile
}

fn run_confirm(profile: &mut Profile, steps: Vec<Step>) -> (ScriptedUi, Result<Confirmed, String>) {
    let mut ui = ScriptedUi::new(steps);
    let confirmed = confirm(&mut ui, profile);
    (ui, confirmed)
}

/// The form with ownership "have written", `clone` checked and answered, and every other
/// category "don't know".
fn ownership_written() -> Step {
    form(&[(
        Category::Ownership,
        Report::HaveWritten,
        &[Element::Clone],
        "I clone strings.",
    )])
}

fn reply(text: &str) -> Result<ChatResponse, ChatError> {
    Ok(ChatResponse {
        content: Some(text.to_owned()),
        tool_calls: Vec::new(),
    })
}

/// A client that judges `clone` known and the rest of ownership unknown.
fn ownership_client() -> FakeChatClient {
    let client = FakeChatClient::new();
    let elements: Vec<_> = Element::ALL
        .iter()
        .filter(|element| element.category() == Category::Ownership)
        .map(|element| {
            let verdict = if *element == Element::Clone {
                "known"
            } else {
                "unknown"
            };
            json!({"element": element.name(), "verdict": verdict, "reason": "from the answer"})
        })
        .collect();
    client.push_chat(reply(&json!({ "elements": elements }).to_string()));
    client
}

/// A temporary directory, and the path of a profile in it that does not exist yet.
fn no_profile() -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("profile.toml");
    (dir, path)
}

fn run_init(
    steps: Vec<Step>,
    client: &FakeChatClient,
    path: &Path,
    repo: Option<&Path>,
) -> (ScriptedUi, Result<Outcome, String>) {
    let mut config = Config::default();
    config.capabilities.light.json_schema = true;
    let router = Router::new(&config);
    let mut ui = ScriptedUi::new(steps);
    let outcome = init(
        &mut ui,
        path,
        Ok(Model {
            client,
            router: &router,
        }),
        repo,
        Language::English,
    );
    (ui, outcome)
}

fn asked_to_confirm(ui: &ScriptedUi) -> usize {
    ui.asked().iter().filter(|asked| *asked == PROMPT).count()
}

// The table.

#[test]
fn the_profile_is_shown_per_category_with_status_provenance_and_the_reason_of_the_judgement() {
    let mut profile = judged_profile();
    let (ui, confirmed) = run_confirm(&mut profile, vec![save()]);
    assert_eq!(confirmed, Ok(Confirmed::Save));
    let shown = ui.shown().join("\n");
    let lines: Vec<&str> = shown.lines().collect();
    let position = |predicate: &dyn Fn(&str) -> bool| {
        lines
            .iter()
            .position(|line| predicate(line))
            .unwrap_or_else(|| panic!("not shown:\n{shown}"))
    };
    let ownership = position(&|line| line == "ownership (report: have_written)");
    let borrowing = position(&|line| line.starts_with("borrowing"));
    let clone = position(&|line| {
        let words: Vec<_> = line.split_whitespace().collect();
        words.starts_with(&["clone", "known", "qa"]) && line.ends_with("clones strings")
    });
    let to_owned = position(&|line| line.split_whitespace().eq(["to_owned", "unknown", "-"]));
    assert!(ownership < clone && clone < borrowing, "{shown}");
    assert!(ownership < to_owned && to_owned < borrowing, "{shown}");
    let crates = position(&|line| line == "crates (reference_repo: /src/app)");
    let tokio = position(&|line| {
        line.split_whitespace()
            .eq(["crate:tokio", "known", "reference_repo"])
    });
    assert!(crates < tokio, "{shown}");
}

#[test]
fn result_and_unwrap_are_shown_in_their_category() {
    let mut profile = Profile::new();
    profile.set_report(Category::ErrorHandling, Report::CanRead);
    for key in [element(Element::Result), element(Element::Unwrap)] {
        profile
            .transition(key, Status::Seen, Provenance::SelfReport, at(1), None)
            .unwrap();
    }
    let (ui, _) = run_confirm(&mut profile, vec![save()]);
    let shown = ui.shown().join("\n");
    let lines: Vec<&str> = shown.lines().collect();
    let find = |words: &[&str]| -> Vec<usize> {
        (0..lines.len())
            .filter(|&i| lines[i].split_whitespace().eq(words.iter().copied()))
            .collect()
    };
    let heading = |text: &str| lines.iter().position(|line| *line == text).unwrap();
    let error_handling = heading("error_handling (report: can_read)");
    let next = heading("iterators_closures");
    for name in ["result", "unwrap"] {
        let row = find(&[name, "seen", "self_report"]);
        assert_eq!(row.len(), 1, "{shown}");
        assert!(error_handling < row[0] && row[0] < next, "{shown}");
    }
    assert!(!shown.contains("regardless of status"), "{shown}");
}

#[test]
fn without_crates_the_crates_heading_is_not_shown() {
    let mut profile = Profile::new();
    profile
        .transition(
            element(Element::Clone),
            Status::Seen,
            Provenance::SelfReport,
            at(1),
            None,
        )
        .unwrap();
    let (ui, _) = run_confirm(&mut profile, vec![save()]);
    let shown = ui.shown().join("\n");
    assert!(
        !shown.lines().any(|line| line.starts_with("crates")),
        "{shown}"
    );
}

#[test]
fn an_empty_line_saves_the_profile_as_it_was_shown() {
    let mut profile = judged_profile();
    let before = profile.clone();
    let (ui, confirmed) = run_confirm(&mut profile, vec![save()]);
    assert_eq!(confirmed, Ok(Confirmed::Save));
    assert_eq!(profile, before);
    assert_eq!(asked_to_confirm(&ui), 1);
}

// Corrections.

#[test]
fn a_correction_in_the_form_of_profile_set_changes_the_item_with_provenance_manual() {
    let mut profile = judged_profile();
    let before = unix_seconds();
    let (ui, confirmed) = run_confirm(
        &mut profile,
        vec![input("clone unknown"), input("  drop   known "), save()],
    );
    let after = unix_seconds();
    assert_eq!(confirmed, Ok(Confirmed::Save));
    assert_eq!(asked_to_confirm(&ui), 3);
    let during: Vec<_> = (before..=after).map(at).collect();
    for (key, status) in [
        (element(Element::Clone), Status::Unknown),
        (element(Element::Drop), Status::Known),
    ] {
        let entry = profile.entry(&key).unwrap();
        assert_eq!(entry.status(), status, "{key}");
        assert_eq!(entry.provenance(), Provenance::Manual, "{key}");
        assert!(during.contains(&entry.at()), "{key}: {}", entry.at());
    }
    assert!(
        ui.shown()
            .iter()
            .any(|line| line == "clone: unknown (manual)"),
        "{:?}",
        ui.shown()
    );
}

#[test]
fn a_crate_can_be_corrected_with_its_key_as_in_profile_set() {
    let mut profile = judged_profile();
    let (_, confirmed) = run_confirm(&mut profile, vec![input("crate:tokio unknown"), save()]);
    assert_eq!(confirmed, Ok(Confirmed::Save));
    let entry = profile.entry(&ItemKey::Crate("tokio".to_owned())).unwrap();
    assert_eq!(entry.status(), Status::Unknown);
    assert_eq!(entry.provenance(), Provenance::Manual);
}

#[test]
fn an_invalid_key_or_status_is_rejected_with_the_valid_inputs_and_asked_again() {
    let mut profile = judged_profile();
    let before = profile.clone();
    let (ui, confirmed) = run_confirm(
        &mut profile,
        vec![input("not_an_element known"), input("clone maybe"), save()],
    );
    assert_eq!(confirmed, Ok(Confirmed::Save));
    assert_eq!(profile, before);
    assert_eq!(asked_to_confirm(&ui), 3);
    let shown = ui.shown().join("\n");
    assert!(shown.contains("not_an_element"), "{shown}");
    assert!(shown.contains("maybe"), "{shown}");
    assert_eq!(shown.matches("valid keys:").count(), 2, "{shown}");
    assert!(
        shown.contains("valid statuses: known, seen, unknown"),
        "{shown}"
    );
}

#[test]
fn a_line_that_is_not_one_key_and_one_status_is_asked_again() {
    let mut profile = judged_profile();
    let before = profile.clone();
    let (ui, confirmed) = run_confirm(
        &mut profile,
        vec![input("clone"), input("clone known now"), save()],
    );
    assert_eq!(confirmed, Ok(Confirmed::Save));
    assert_eq!(profile, before);
    assert_eq!(asked_to_confirm(&ui), 3);
}

// Through init: what is saved.

#[test]
fn a_correction_at_the_confirmation_is_saved_with_provenance_manual() {
    let (_dir, path) = no_profile();
    let steps = vec![
        ownership_written(),
        input("clone unknown"),
        input("mem_take known"),
        save(),
    ];
    let (_, outcome) = run_init(steps, &ownership_client(), &path, None);
    assert_eq!(outcome, Ok(Outcome::Saved));

    let profile = Profile::load(&path).unwrap();
    let clone = profile.entry(&element(Element::Clone)).unwrap();
    assert_eq!(
        (clone.status(), clone.provenance()),
        (Status::Unknown, Provenance::Manual)
    );
    let mem_take = profile.entry(&element(Element::MemTake)).unwrap();
    assert_eq!(
        (mem_take.status(), mem_take.provenance()),
        (Status::Known, Provenance::Manual)
    );
    let last: Vec<_> = profile
        .history()
        .iter()
        .rev()
        .take(2)
        .map(|change| (change.key().to_string(), change.provenance()))
        .collect();
    assert_eq!(
        last,
        [
            ("mem_take".to_owned(), Provenance::Manual),
            ("clone".to_owned(), Provenance::Manual)
        ]
    );
}

#[test]
fn every_known_item_saved_is_by_the_q_and_a_the_reference_repository_or_manual_with_its_time() {
    let (dir, path) = no_profile();
    let repo = dir.path().join("repo");
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(repo.join("Cargo.toml"), "[dependencies]\ntokio = \"1\"\n").unwrap();
    fs::write(repo.join("src/main.rs"), "fn main() {}\n").unwrap();
    let steps = vec![
        form(&[
            (
                Category::Ownership,
                Report::HaveWritten,
                &[Element::Clone],
                "I clone strings.",
            ),
            (Category::SmartPointers, Report::CanRead, &[], ""),
        ]),
        input("arc known"),
        save(),
    ];

    let before = unix_seconds();
    let (_, outcome) = run_init(steps, &ownership_client(), &path, Some(&repo));
    let after = unix_seconds();
    assert_eq!(outcome, Ok(Outcome::Saved));
    let during: Vec<_> = (before..=after).map(at).collect();

    let profile = Profile::load(&path).unwrap();
    let mut provenances = Vec::new();
    for (key, entry) in profile.entries() {
        assert!(during.contains(&entry.at()), "{key}: {}", entry.at());
        if entry.status() == Status::Known {
            provenances.push(entry.provenance());
        }
    }
    for provenance in &provenances {
        assert!(
            matches!(
                provenance,
                Provenance::Qa | Provenance::ReferenceRepo | Provenance::Manual
            ),
            "{provenances:?}"
        );
    }
    for expected in [
        Provenance::Qa,
        Provenance::ReferenceRepo,
        Provenance::Manual,
    ] {
        assert!(provenances.contains(&expected), "{provenances:?}");
    }
}

// Through init: what is not saved.

#[test]
fn interrupting_the_confirmation_saves_nothing() {
    let (_dir, path) = no_profile();
    let steps = vec![ownership_written(), input("clone unknown"), Step::Interrupt];
    let (ui, outcome) = run_init(steps, &ownership_client(), &path, None);
    assert_eq!(outcome, Ok(Outcome::Interrupted));
    assert!(!path.exists());
    assert_eq!(asked_to_confirm(&ui), 2);
}

#[test]
fn the_end_of_the_input_at_the_confirmation_saves_nothing_as_an_interruption() {
    let (_dir, path) = no_profile();
    let steps = vec![form(&[]), Step::Eof];
    let (_, outcome) = run_init(steps, &FakeChatClient::new(), &path, None);
    assert_eq!(outcome, Ok(Outcome::Interrupted));
    assert!(!path.exists());
}

#[test]
fn interrupting_the_confirmation_leaves_an_existing_profile_as_it_was() {
    let (_dir, path) = no_profile();
    let existing = "# written by hand\n[items.arc]\nstatus = \"known\"\nprovenance = \"manual\"\nat = 2026-10-10T12:34:56Z\n";
    fs::write(&path, existing).unwrap();
    let steps = vec![Step::Confirm(true), form(&[]), Step::Interrupt];
    let (_, outcome) = run_init(steps, &FakeChatClient::new(), &path, None);
    assert_eq!(outcome, Ok(Outcome::Interrupted));
    assert_eq!(fs::read_to_string(&path).unwrap(), existing);
}

#[test]
fn a_failure_to_read_at_the_confirmation_reports_it_and_saves_nothing() {
    let (_dir, path) = no_profile();
    let (_, outcome) = run_init(vec![form(&[])], &FakeChatClient::new(), &path, None);
    let error = outcome.unwrap_err();
    assert!(error.contains("no step left"), "{error}");
    assert!(!path.exists());
}

#[test]
fn an_api_failure_during_the_judgement_asks_no_confirmation_and_tells_the_cause() {
    let (_dir, path) = no_profile();
    let client = FakeChatClient::new();
    client.push_chat(Err(ChatError::Status {
        status: 429,
        body: "rate limited".to_owned(),
    }));
    // The form opens again; the user quits there.
    let (ui, outcome) = run_init(
        vec![ownership_written(), Step::Interrupt],
        &client,
        &path,
        None,
    );
    assert_eq!(outcome, Ok(Outcome::Interrupted));
    assert!(
        ui.shown().iter().any(|shown| shown.contains("429")),
        "{:?}",
        ui.shown()
    );
    assert_eq!(asked_to_confirm(&ui), 0);
    assert!(!path.exists());
}
