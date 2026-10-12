use std::fs;
use std::path::PathBuf;

use tempfile::TempDir;

use super::*;
use crate::config::{Api, Config};
use crate::llm::{ChatClient, FakeChatClient, OpenAiClient, Router};
use crate::onboarding::form::Language;
use crate::onboarding::questionnaire::test_support::{every, form};
use crate::profile::element::{Category, Element};
use crate::profile::status::{ItemKey, Provenance, Status};
use crate::profile::store::{Profile, Report};
use crate::ui::{ScriptedUi, Step};

/// A temporary directory, and the path of a profile in it that does not exist yet.
fn no_profile() -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("profile.toml");
    (dir, path)
}

/// A profile file that `init` would rewrite if it saved (the comment would be dropped).
const EXISTING_PROFILE: &str = "# written by hand\ndeny = [\"option\"]\n";

fn existing_profile() -> (TempDir, PathBuf) {
    let (dir, path) = no_profile();
    fs::write(&path, EXISTING_PROFILE).unwrap();
    (dir, path)
}

/// The empty line that saves at the confirmation; its corrections are tested in
/// `onboarding::confirm`.
fn save() -> Step {
    Step::Input(String::new())
}

/// The steps that submit `report` for every category, and then save at the confirmation.
fn every_category(report: Report) -> Vec<Step> {
    vec![every(report), save()]
}

/// The steps that submit `report` for `category` and "don't know" for every other category, and
/// then save at the confirmation. "have written" is tested in `onboarding::judge`.
fn only(category: Category, report: Report) -> Vec<Step> {
    vec![form(&[(category, report, &[], "")]), save()]
}

fn elements_of(category: Category) -> impl Iterator<Item = Element> {
    Element::ALL
        .iter()
        .copied()
        .filter(move |element| element.category() == category)
}

fn run(ui: &mut ScriptedUi, path: &Path, client: &FakeChatClient) -> Result<Outcome, String> {
    let router = Router::new(&Config::default());
    init(
        ui,
        path,
        Ok(Model {
            client,
            router: &router,
        }),
        None,
        Language::English,
    )
}

#[test]
fn the_form_has_the_ten_categories_in_order_and_is_followed_by_the_confirmation() {
    let (_dir, path) = no_profile();
    let mut ui = ScriptedUi::new(every_category(Report::DontKnow));
    run(&mut ui, &path, &FakeChatClient::new()).unwrap();
    let titles: Vec<&str> = ui.forms()[0]
        .form
        .tabs
        .iter()
        .map(|tab| tab.title.as_str())
        .collect();
    let names: Vec<&str> = Category::ALL
        .iter()
        .map(|category| category.name())
        .collect();
    assert_eq!(titles, names);
    assert_eq!(
        ui.asked().len(),
        2,
        "the form and the confirmation: {:?}",
        ui.asked()
    );
}

#[test]
fn can_read_makes_the_elements_of_the_category_seen_by_self_report() {
    let (_dir, path) = no_profile();
    let mut ui = ScriptedUi::new(only(Category::SmartPointers, Report::CanRead));
    assert_eq!(
        run(&mut ui, &path, &FakeChatClient::new()),
        Ok(Outcome::Saved)
    );
    let profile = Profile::load(&path).unwrap();
    for element in elements_of(Category::SmartPointers) {
        let entry = profile.entry(&ItemKey::Element(element));
        let entry = entry.unwrap_or_else(|| panic!("no entry for {}", element.name()));
        assert_eq!(entry.status(), Status::Seen, "{}", element.name());
        assert_eq!(
            entry.provenance(),
            Provenance::SelfReport,
            "{}",
            element.name()
        );
    }
    assert_eq!(
        profile.report(Category::SmartPointers),
        Some(Report::CanRead)
    );
}

#[test]
fn dont_know_leaves_the_elements_of_the_category_unknown() {
    let (_dir, path) = no_profile();
    let mut ui = ScriptedUi::new(only(Category::SmartPointers, Report::CanRead));
    run(&mut ui, &path, &FakeChatClient::new()).unwrap();
    let profile = Profile::load(&path).unwrap();
    for category in Category::ALL
        .into_iter()
        .filter(|&c| c != Category::SmartPointers)
    {
        for element in elements_of(category) {
            assert_eq!(
                profile.status(&ItemKey::Element(element)),
                Status::Unknown,
                "{}",
                element.name()
            );
        }
        assert_eq!(profile.report(category), Some(Report::DontKnow));
    }
}

#[test]
fn all_dont_know_saves_a_profile_of_base_elements_only_without_calling_the_model() {
    let (_dir, path) = no_profile();
    let client = FakeChatClient::new();
    let mut ui = ScriptedUi::new(every_category(Report::DontKnow));
    assert_eq!(run(&mut ui, &path, &client), Ok(Outcome::Saved));
    assert!(client.requests().is_empty(), "{:?}", client.requests());
    let profile = Profile::load(&path).unwrap();
    assert_eq!(profile.entries().count(), 0);
    assert!(profile.allow().is_empty() && profile.deny().is_empty());
    assert!(profile.history().is_empty());
}

#[test]
fn the_profile_directory_is_created_when_it_does_not_exist() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("molt").join("profile.toml");
    let mut ui = ScriptedUi::new(every_category(Report::DontKnow));
    assert_eq!(
        run(&mut ui, &path, &FakeChatClient::new()),
        Ok(Outcome::Saved)
    );
    assert!(path.is_file());
}

#[test]
fn without_the_api_key_nothing_is_asked_and_no_profile_is_created() {
    let api = Api {
        base_url: "https://example.invalid/v1".to_owned(),
        api_key_env: "MOLT_TEST_MISSING_API_KEY".to_owned(),
        timeout_secs: 1,
    };
    let client = OpenAiClient::new(&api, |_| None).map_err(|error| error.to_string());
    let router = Router::new(&Config::default());
    let (_dir, path) = no_profile();
    let mut ui = ScriptedUi::new(every_category(Report::DontKnow));
    let result = init(
        &mut ui,
        &path,
        client
            .as_ref()
            .map(|c| Model {
                client: c as &dyn ChatClient,
                router: &router,
            })
            .map_err(Clone::clone),
        None,
        Language::English,
    );
    let error = result.unwrap_err();
    assert!(error.contains("MOLT_TEST_MISSING_API_KEY"), "{error}");
    assert!(ui.asked().is_empty(), "{:?}", ui.asked());
    assert!(!path.exists());
}

#[test]
fn declining_to_overwrite_an_existing_profile_changes_nothing() {
    let (_dir, path) = existing_profile();
    let mut steps = vec![Step::Confirm(false)];
    steps.extend(every_category(Report::DontKnow));
    let mut ui = ScriptedUi::new(steps);
    assert_eq!(
        run(&mut ui, &path, &FakeChatClient::new()),
        Ok(Outcome::Kept)
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), EXISTING_PROFILE);
    assert_eq!(ui.asked().len(), 1, "only the overwrite is asked");
}

#[test]
fn interrupting_the_overwrite_question_changes_nothing() {
    let (_dir, path) = existing_profile();
    let mut ui = ScriptedUi::new([Step::Interrupt]);
    assert_eq!(
        run(&mut ui, &path, &FakeChatClient::new()),
        Ok(Outcome::Interrupted)
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), EXISTING_PROFILE);
}

#[test]
fn accepting_to_overwrite_replaces_the_existing_profile() {
    let (_dir, path) = existing_profile();
    let mut steps = vec![Step::Confirm(true)];
    steps.extend(every_category(Report::DontKnow));
    let mut ui = ScriptedUi::new(steps);
    assert_eq!(
        run(&mut ui, &path, &FakeChatClient::new()),
        Ok(Outcome::Saved)
    );
    let profile = Profile::load(&path).unwrap();
    assert!(profile.deny().is_empty(), "the old profile is gone");
}

#[test]
fn interrupting_the_form_saves_nothing() {
    let (_dir, path) = no_profile();
    let mut ui = ScriptedUi::new([Step::Interrupt]);
    assert_eq!(
        run(&mut ui, &path, &FakeChatClient::new()),
        Ok(Outcome::Interrupted)
    );
    assert!(!path.exists());
}
