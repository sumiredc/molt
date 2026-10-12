use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use serde_json::json;
use tempfile::TempDir;

use super::*;
use crate::config::Config;
use crate::llm::FakeChatClient;
use crate::llm::types::{ChatRequest, ChatResponse, ResponseFormat};
use crate::onboarding::form::Language;
use crate::onboarding::questionnaire::test_support::form;
use crate::onboarding::questionnaire::{Outcome, init};
use crate::profile::status::{ItemKey, Provenance, Status};
use crate::profile::store::{Profile, Report};
use crate::ui::{ScriptedUi, Step};

/// The model ids of the roles in [`config`], to tell the requests apart.
const LIGHT: &str = "light-model";

/// A config whose `light` role takes `json_schema` when `json_schema` is true, and otherwise
/// answers in `json_object` with the one repair.
fn config(json_schema: bool) -> Config {
    let mut config = Config::default();
    config.models.light = LIGHT.to_owned();
    config.capabilities.light.json_schema = json_schema;
    config
}

fn reply(text: &str) -> Result<ChatResponse, ChatError> {
    Ok(ChatResponse {
        content: Some(text.to_owned()),
        tool_calls: Vec::new(),
    })
}

/// The judgement giving each element its verdict with the reason `because <element>`.
fn judgement(elements: &[(Element, &str)]) -> Result<ChatResponse, ChatError> {
    let elements: Vec<_> = elements
        .iter()
        .map(|(element, verdict)| {
            json!({
                "element": element.name(),
                "verdict": verdict,
                "reason": format!("because {}", element.name()),
            })
        })
        .collect();
    reply(&json!({ "elements": elements }).to_string())
}

/// What a judgement request about `category` contains, to answer it whichever order it comes in.
fn about(category: Category) -> String {
    format!("The category is `{category}`.")
}

/// The empty line that saves at the confirmation.
fn save() -> Step {
    Step::Input(String::new())
}

/// A temporary directory, and the path of a profile in it that does not exist yet.
fn no_profile() -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("profile.toml");
    (dir, path)
}

fn run_init(
    steps: Vec<Step>,
    client: &dyn ChatClient,
    config: &Config,
    path: &Path,
    language: Language,
) -> (ScriptedUi, Result<Outcome, String>) {
    let router = Router::new(config);
    let mut ui = ScriptedUi::new(steps);
    let outcome = init(
        &mut ui,
        path,
        Ok(Model {
            client,
            router: &router,
        }),
        None,
        language,
    );
    (ui, outcome)
}

fn user_text(request: &ChatRequest) -> String {
    request
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::User { content } => Some(content.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn system_text(request: &ChatRequest) -> String {
    match request.messages.first() {
        Some(Message::System { content }) => content.clone(),
        other => panic!("no system message first: {other:?}"),
    }
}

const OWNERSHIP_ANSWER: &str = "I clone strings, and mem::take a Vec out of a struct.";

/// The form with ownership "have written", `clone` and `mem_take` checked and answered.
fn ownership_written() -> Step {
    form(&[(
        Category::Ownership,
        Report::HaveWritten,
        &[Element::Clone, Element::MemTake],
        OWNERSHIP_ANSWER,
    )])
}

#[test]
fn checked_elements_are_known_by_qa_or_unknown_as_judged_and_the_others_unknown() {
    let (_dir, path) = no_profile();
    let client = FakeChatClient::new();
    client.push_chat_when(
        about(Category::Ownership),
        judgement(&[(Element::Clone, "known"), (Element::MemTake, "unknown")]),
    );
    let (_, outcome) = run_init(
        vec![ownership_written(), save()],
        &client,
        &config(true),
        &path,
        Language::English,
    );
    assert_eq!(outcome, Ok(Outcome::Saved));

    let profile = Profile::load(&path).unwrap();
    let clone = profile.entry(&ItemKey::Element(Element::Clone)).unwrap();
    assert_eq!(clone.status(), Status::Known);
    assert_eq!(clone.provenance(), Provenance::Qa);
    assert_eq!(clone.evidence(), Some("because clone"));
    for element in Category::Ownership
        .elements()
        .filter(|&e| e != Element::Clone)
    {
        assert_eq!(
            profile.status(&ItemKey::Element(element)),
            Status::Unknown,
            "{}",
            element.name()
        );
    }
    assert_eq!(
        profile.report(Category::Ownership),
        Some(Report::HaveWritten)
    );
}

#[test]
fn the_request_judges_only_the_checked_elements_with_the_light_model_and_quotes_the_answer_last() {
    let (_dir, path) = no_profile();
    let client = FakeChatClient::new();
    client.push_chat_when(about(Category::Ownership), judgement(&[]));
    run_init(
        vec![ownership_written(), save()],
        &client,
        &config(true),
        &path,
        Language::English,
    )
    .1
    .unwrap();

    let requests = client.requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.model, LIGHT);
    assert_eq!(system_text(request), prompts::JUDGE_SYSTEM);
    assert!(matches!(
        &request.response_format,
        Some(ResponseFormat::JsonSchema { .. })
    ));
    let text = user_text(request);
    assert!(text.contains("- `clone`\n- `mem_take`\n\n"), "{text}");
    assert!(!text.contains("`to_owned`"), "{text}");
    assert!(
        text.contains("Question: Where have you used clone, mem_take?"),
        "the question as the user saw it: {text}"
    );
    assert!(
        text.ends_with(&format!("The user's latest input was:\n{OWNERSHIP_ANSWER}")),
        "{text}"
    );
}

#[test]
fn a_blank_answer_is_not_sent_and_its_checked_elements_stay_unknown() {
    let (_dir, path) = no_profile();
    let client = FakeChatClient::new();
    let (_, outcome) = run_init(
        vec![
            form(&[(
                Category::Ownership,
                Report::HaveWritten,
                &[Element::Clone],
                "  ",
            )]),
            save(),
        ],
        &client,
        &config(true),
        &path,
        Language::English,
    );
    assert_eq!(outcome, Ok(Outcome::Saved));
    assert!(client.requests().is_empty(), "{:?}", client.requests());
    let profile = Profile::load(&path).unwrap();
    assert_eq!(
        profile.status(&ItemKey::Element(Element::Clone)),
        Status::Unknown
    );
}

#[test]
fn one_request_per_category_sent_each_answered_by_its_category() {
    let (_dir, path) = no_profile();
    let client = FakeChatClient::new();
    client.push_chat_when(
        about(Category::Borrowing),
        judgement(&[(Element::MutRef, "known")]),
    );
    client.push_chat_when(
        about(Category::Ownership),
        judgement(&[(Element::Clone, "known")]),
    );
    let (ui, outcome) = run_init(
        vec![
            form(&[
                (
                    Category::Ownership,
                    Report::HaveWritten,
                    &[Element::Clone],
                    "ownership answer",
                ),
                (
                    Category::Borrowing,
                    Report::HaveWritten,
                    &[Element::MutRef],
                    "borrowing answer",
                ),
            ]),
            save(),
        ],
        &client,
        &config(true),
        &path,
        Language::English,
    );
    assert_eq!(outcome, Ok(Outcome::Saved));
    assert_eq!(client.requests().len(), 2);
    let profile = Profile::load(&path).unwrap();
    assert_eq!(
        profile.status(&ItemKey::Element(Element::Clone)),
        Status::Known
    );
    assert_eq!(
        profile.status(&ItemKey::Element(Element::MutRef)),
        Status::Known
    );
    // The results are told in the order of the categories.
    let shown = ui.shown().join("\n");
    let ownership = shown.find("ownership: known: clone").unwrap();
    let borrowing = shown.find("borrowing: known: mut_ref").unwrap();
    assert!(ownership < borrowing, "{shown}");
}

/// A [`ChatClient`] that holds each request a while and counts how many it holds at once.
struct Holding {
    inner: FakeChatClient,
    now: AtomicUsize,
    most: AtomicUsize,
}

impl ChatClient for Holding {
    fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, ChatError> {
        let now = self.now.fetch_add(1, Ordering::SeqCst) + 1;
        self.most.fetch_max(now, Ordering::SeqCst);
        thread::sleep(Duration::from_millis(50));
        self.now.fetch_sub(1, Ordering::SeqCst);
        self.inner.chat(request)
    }

    fn models(&self) -> Result<Vec<String>, ChatError> {
        self.inner.models()
    }
}

#[test]
fn at_most_three_requests_are_sent_at_a_time() {
    let (_dir, path) = no_profile();
    let categories = [
        Category::Ownership,
        Category::Borrowing,
        Category::Lifetimes,
        Category::TraitsGenerics,
        Category::SmartPointers,
    ];
    let client = Holding {
        inner: FakeChatClient::new(),
        now: AtomicUsize::new(0),
        most: AtomicUsize::new(0),
    };
    let given: Vec<(Category, Report, Vec<Element>, &str)> = categories
        .iter()
        .map(|&category| {
            client.inner.push_chat_when(about(category), judgement(&[]));
            let first = category.elements().next().unwrap();
            (category, Report::HaveWritten, vec![first], "an answer")
        })
        .collect();
    let given: Vec<(Category, Report, &[Element], &str)> = given
        .iter()
        .map(|(category, report, checked, answer)| {
            (*category, *report, checked.as_slice(), *answer)
        })
        .collect();

    let (_, outcome) = run_init(
        vec![form(&given), save()],
        &client,
        &config(true),
        &path,
        Language::English,
    );
    assert_eq!(outcome, Ok(Outcome::Saved));
    assert_eq!(client.inner.requests().len(), 5);
    assert_eq!(client.most.load(Ordering::SeqCst), PARALLEL);
}

#[test]
fn an_api_failure_opens_the_form_again_as_submitted_and_a_resubmission_judges_all_again() {
    let (_dir, path) = no_profile();
    let client = FakeChatClient::new();
    client.push_chat_when(about(Category::Ownership), Err(ChatError::FakeExhausted));
    client.push_chat_when(about(Category::Borrowing), judgement(&[]));
    client.push_chat_when(
        about(Category::Ownership),
        judgement(&[(Element::Clone, "known")]),
    );
    client.push_chat_when(
        about(Category::Borrowing),
        judgement(&[(Element::MutRef, "known")]),
    );
    let submitted = form(&[
        (
            Category::Ownership,
            Report::HaveWritten,
            &[Element::Clone],
            "ownership answer",
        ),
        (
            Category::Borrowing,
            Report::HaveWritten,
            &[Element::MutRef],
            "borrowing answer",
        ),
        // Checks and an answer kept under another report are not sent, and come back too.
        (
            Category::Lifetimes,
            Report::CanRead,
            &[Element::StaticLifetime],
            "kept",
        ),
    ]);
    let (ui, outcome) = run_init(
        vec![submitted.clone(), submitted, save()],
        &client,
        &config(true),
        &path,
        Language::English,
    );
    assert_eq!(outcome, Ok(Outcome::Saved));

    let forms = ui.forms();
    assert_eq!(forms.len(), 2, "the form opened again");
    assert_eq!(forms[0].initial, None);
    let initial = forms[1].initial.as_ref().unwrap();
    assert_eq!(initial.tabs[0].answer, "ownership answer");
    assert_eq!(initial.tabs[2].choice, Some(1));
    assert_eq!(initial.tabs[2].answer, "kept");
    assert_eq!(initial.tabs[2].checked.len(), 1);
    assert!(
        ui.shown()
            .iter()
            .any(|shown| shown.contains("the judgement of ownership failed")),
        "{:?}",
        ui.shown()
    );

    // 2 categories, judged twice; or the failed one alone the first time.
    let requests = client.requests().len();
    assert!((3..=4).contains(&requests), "{requests}");
    let profile = Profile::load(&path).unwrap();
    assert_eq!(
        profile.status(&ItemKey::Element(Element::Clone)),
        Status::Known
    );
    assert_eq!(
        profile.status(&ItemKey::Element(Element::MutRef)),
        Status::Known
    );
}

#[test]
fn interrupting_the_reopened_form_saves_nothing() {
    let (_dir, path) = no_profile();
    let client = FakeChatClient::new();
    client.push_chat_when(about(Category::Ownership), Err(ChatError::FakeExhausted));
    let (_, outcome) = run_init(
        vec![ownership_written(), Step::Interrupt],
        &client,
        &config(true),
        &path,
        Language::English,
    );
    assert_eq!(outcome, Ok(Outcome::Interrupted));
    assert!(!path.exists());
}

#[test]
fn a_judgement_that_does_not_fit_leaves_its_elements_unknown_without_opening_the_form_again() {
    let (_dir, path) = no_profile();
    let client = FakeChatClient::new();
    client.push_chat_when(about(Category::Ownership), reply("not json"));
    let (ui, outcome) = run_init(
        vec![ownership_written(), save()],
        &client,
        &config(true),
        &path,
        Language::English,
    );
    assert_eq!(outcome, Ok(Outcome::Saved));
    assert_eq!(ui.forms().len(), 1);
    assert_eq!(client.requests().len(), 1, "no repair with json_schema");
    assert!(
        ui.shown()
            .iter()
            .any(|shown| shown.starts_with("Could not judge the answer about ownership")),
        "{:?}",
        ui.shown()
    );
    let profile = Profile::load(&path).unwrap();
    assert_eq!(
        profile.status(&ItemKey::Element(Element::Clone)),
        Status::Unknown
    );
}

#[test]
fn without_json_schema_a_judgement_that_fits_after_the_repair_is_used() {
    let (_dir, path) = no_profile();
    let client = FakeChatClient::new();
    client.push_chat_when(about(Category::Ownership), reply("not json"));
    // The repair repeats the request, and so its category, with the wrong reply after it.
    client.push_chat_when(
        about(Category::Ownership),
        judgement(&[(Element::Clone, "known")]),
    );
    let (_, outcome) = run_init(
        vec![ownership_written(), save()],
        &client,
        &config(false),
        &path,
        Language::English,
    );
    assert_eq!(outcome, Ok(Outcome::Saved));
    assert_eq!(client.requests().len(), 2);
    let profile = Profile::load(&path).unwrap();
    assert_eq!(
        profile.status(&ItemKey::Element(Element::Clone)),
        Status::Known
    );
}

#[test]
fn an_element_the_judgement_leaves_unclear_or_out_or_does_not_list_is_not_known() {
    let (_dir, path) = no_profile();
    let client = FakeChatClient::new();
    client.push_chat_when(
        about(Category::Ownership),
        reply(
            &json!({ "elements": [
                { "element": "clone", "verdict": "unclear", "reason": "vague" },
                { "element": "to_owned", "verdict": "known", "reason": "not checked" },
            ] })
            .to_string(),
        ),
    );
    let (_, outcome) = run_init(
        vec![ownership_written(), save()],
        &client,
        &config(false),
        &path,
        Language::English,
    );
    assert_eq!(outcome, Ok(Outcome::Saved));
    let profile = Profile::load(&path).unwrap();
    for element in [Element::Clone, Element::MemTake, Element::ToOwned] {
        assert_eq!(
            profile.status(&ItemKey::Element(element)),
            Status::Unknown,
            "{}",
            element.name()
        );
    }
}

#[test]
fn an_interruption_while_judging_saves_nothing() {
    let (_dir, path) = no_profile();
    let client = FakeChatClient::new();
    client.push_chat_when(about(Category::Ownership), judgement(&[]));
    client.push_chat_when(about(Category::Borrowing), Err(ChatError::Interrupted));
    let (ui, outcome) = run_init(
        vec![form(&[
            (
                Category::Ownership,
                Report::HaveWritten,
                &[Element::Clone],
                "ownership answer",
            ),
            (
                Category::Borrowing,
                Report::HaveWritten,
                &[Element::MutRef],
                "borrowing answer",
            ),
        ])],
        &client,
        &config(true),
        &path,
        Language::English,
    );
    assert_eq!(outcome, Ok(Outcome::Interrupted));
    assert_eq!(ui.forms().len(), 1, "the form does not open again");
    assert!(!path.exists());
}

#[test]
fn interrupting_the_form_sends_nothing_and_saves_nothing() {
    let (_dir, path) = no_profile();
    let client = FakeChatClient::new();
    let (_, outcome) = run_init(
        vec![Step::Interrupt],
        &client,
        &config(true),
        &path,
        Language::English,
    );
    assert_eq!(outcome, Ok(Outcome::Interrupted));
    assert!(client.requests().is_empty());
    assert!(!path.exists());
}

#[test]
fn the_form_is_in_the_language_given() {
    let (_dir, path) = no_profile();
    let client = FakeChatClient::new();
    let (ui, _) = run_init(
        vec![Step::Interrupt],
        &client,
        &config(true),
        &path,
        Language::Japanese,
    );
    let form = &ui.forms()[0].form;
    assert_eq!(form.tabs[0].title, "所有権");
    assert_eq!(form.text.submit_title, "送信");
}

/// Keeps the requests of a judgement and replies with an empty judgement.
#[derive(Default)]
struct Recording {
    requests: Mutex<Vec<ChatRequest>>,
}

impl ChatClient for Recording {
    fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, ChatError> {
        self.requests.lock().unwrap().push(request.clone());
        judgement(&[])
    }

    fn models(&self) -> Result<Vec<String>, ChatError> {
        Ok(Vec::new())
    }
}

#[test]
fn a_japanese_answer_is_quoted_as_the_user_s_latest_input() {
    let (_dir, path) = no_profile();
    let client = Recording::default();
    let answer = "文字列を clone した。";
    run_init(
        vec![
            form(&[(
                Category::Ownership,
                Report::HaveWritten,
                &[Element::Clone],
                answer,
            )]),
            save(),
        ],
        &client,
        &config(true),
        &path,
        Language::Japanese,
    )
    .1
    .unwrap();
    let requests = client.requests.lock().unwrap();
    let text = user_text(&requests[0]);
    assert!(
        text.contains("Question: clone をどこで使いましたか？"),
        "{text}"
    );
    assert!(text.ends_with(answer), "{text}");
}
