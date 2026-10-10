use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::json;
use tempfile::TempDir;

use super::*;
use crate::cli::ui::{ScriptedUi, Step};
use crate::config::Config;
use crate::llm::FakeChatClient;
use crate::llm::types::{ChatRequest, ChatResponse, ResponseFormat};
use crate::onboarding::questionnaire::{Outcome, init};
use crate::onboarding::repo_scan::Place;
use crate::profile::status::{ItemKey, Provenance, Status};
use crate::profile::store::{Profile, Report, Timestamp};

const HAVE_WRITTEN: Step = Step::Select(1);
const DONT_KNOW: Step = Step::Select(3);

/// The model ids of the roles in [`config`], to tell the requests apart.
const LIGHT: &str = "light-model";
const REASON: &str = "reason-model";

/// The line of the system prompts that keeps the user's language (PRD §3 Language).
const LANGUAGE: &str = "Answer in the language of the user's latest input.";

/// A config whose `light` role takes `json_schema` when `json_schema` is true, and otherwise
/// answers in `json_object` with the one repair.
fn config(json_schema: bool) -> Config {
    let mut config = Config::default();
    config.models.light = LIGHT.to_owned();
    config.models.reason = REASON.to_owned();
    config.capabilities.light.json_schema = json_schema;
    config
}

fn elements_of(category: Category) -> Vec<Element> {
    Element::ALL
        .iter()
        .copied()
        .filter(|element| element.category() == category)
        .collect()
}

fn reply(text: &str) -> Result<ChatResponse, ChatError> {
    Ok(ChatResponse {
        content: Some(text.to_owned()),
        tool_calls: Vec::new(),
    })
}

/// The judgement giving `verdict` with the reason `because <element>` to each of `elements`.
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

/// `verdict` for every element of `category`.
fn all(category: Category, verdict: &str) -> Vec<(Element, &str)> {
    elements_of(category)
        .into_iter()
        .map(|element| (element, verdict))
        .collect()
}

/// The steps that report `have_written` "have written" and the others "don't know".
fn reports(have_written: &[Category]) -> Vec<Step> {
    Category::ALL
        .iter()
        .map(|category| {
            if have_written.contains(category) {
                HAVE_WRITTEN
            } else {
                DONT_KNOW
            }
        })
        .collect()
}

fn answer(text: &str) -> Step {
    Step::Input(text.to_owned())
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
    config: &Config,
    path: &Path,
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
    );
    (ui, outcome)
}

fn run_probe(
    steps: Vec<Step>,
    client: &FakeChatClient,
    config: &Config,
    categories: &[Category],
    scan: Option<&RepoScan>,
) -> (ScriptedUi, Result<Vec<Judgement>, Stopped>) {
    let router = Router::new(config);
    let mut ui = ScriptedUi::new(steps);
    let judged = probe(
        &mut ui,
        Model {
            client,
            router: &router,
        },
        categories,
        scan,
    );
    (ui, judged)
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

/// The requests for questions (to the `reason` model) about `category`.
fn questions_about(client: &FakeChatClient, category: Category) -> Vec<ChatRequest> {
    let tag = format!("The category is `{category}`.");
    client
        .requests()
        .into_iter()
        .filter(|request| request.model == REASON && user_text(request).contains(&tag))
        .collect()
}

fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn known(judged: &[Judgement]) -> Vec<Element> {
    judged
        .iter()
        .filter(|judgement| judgement.known)
        .map(|judgement| judgement.element)
        .collect()
}

#[test]
fn the_elements_of_a_have_written_category_are_known_by_qa_or_unknown_as_judged() {
    let (_dir, path) = no_profile();
    let client = FakeChatClient::new();
    client.push_chat(reply("How have you used clone and the mem functions?"));
    client.push_chat(judgement(&[
        (Element::Clone, "known"),
        (Element::ToOwned, "known"),
        (Element::Drop, "unknown"),
        (Element::MemTake, "unknown"),
        (Element::MemReplace, "unknown"),
        (Element::MemSwap, "unknown"),
    ]));
    let mut steps = reports(&[Category::Ownership]);
    steps.push(answer("I clone strings and call to_owned on &str."));
    // Saves at the confirmation as judged.
    steps.push(answer(""));

    let before = unix_seconds();
    let (_, outcome) = run_init(steps, &client, &config(true), &path);
    let after = unix_seconds();
    assert_eq!(outcome, Ok(Outcome::Saved));
    let during: Vec<_> = (before..=after).map(Timestamp::from_unix_seconds).collect();

    let profile = Profile::load(&path).unwrap();
    assert_eq!(
        profile.report(Category::Ownership),
        Some(Report::HaveWritten)
    );
    for element in [Element::Clone, Element::ToOwned] {
        let entry = profile.entry(&ItemKey::Element(element)).unwrap();
        assert_eq!(entry.status(), Status::Known, "{}", element.name());
        assert_eq!(entry.provenance(), Provenance::Qa, "{}", element.name());
        assert!(during.contains(&entry.at()), "{:?}", entry.at());
        assert_eq!(
            entry.evidence(),
            Some(format!("because {}", element.name()).as_str())
        );
    }
    for element in [
        Element::Drop,
        Element::MemTake,
        Element::MemReplace,
        Element::MemSwap,
    ] {
        assert_eq!(
            profile.status(&ItemKey::Element(element)),
            Status::Unknown,
            "{}",
            element.name()
        );
    }
    assert_eq!(profile.entries().count(), 2);
}

#[test]
fn only_the_unclear_elements_get_one_follow_up_question_and_are_judged_again() {
    let client = FakeChatClient::new();
    client.push_chat(reply("Question 1"));
    let mut first: Vec<_> = all(Category::Ownership, "unknown");
    first[0] = (Element::Clone, "known");
    first[1] = (Element::ToOwned, "unclear");
    client.push_chat(judgement(&first));
    client.push_chat(reply("Question 2"));
    client.push_chat(judgement(&[(Element::ToOwned, "known")]));

    let steps = vec![answer("answer 1"), answer("answer 2")];
    let (ui, judged) = run_probe(steps, &client, &config(true), &[Category::Ownership], None);
    assert_eq!(known(&judged.unwrap()), [Element::Clone, Element::ToOwned]);

    let requests = client.requests();
    assert_eq!(requests.len(), 4, "{requests:?}");
    let follow_up = user_text(&requests[2]);
    assert!(follow_up.contains("`to_owned`"), "{follow_up}");
    assert!(!follow_up.contains("`clone`"), "{follow_up}");
    assert!(follow_up.contains("answer 1"), "{follow_up}");
    let second_judgement = user_text(&requests[3]);
    assert!(
        second_judgement.contains("`to_owned`"),
        "{second_judgement}"
    );
    assert!(!second_judgement.contains("`clone`"), "{second_judgement}");
    assert!(ui.shown().contains(&"Question 2".to_owned()));
}

#[test]
fn a_category_gets_one_question_and_at_most_one_follow_up() {
    let client = FakeChatClient::new();
    // Every judgement leaves every element unclear, and more questions are queued than may be
    // asked: the budget, not the queue, ends each category.
    for category in [Category::Ownership, Category::Borrowing] {
        for round in 0..2 {
            client.push_chat(reply(&format!("{category} question {round}")));
            client.push_chat(judgement(&all(category, "unclear")));
        }
    }
    client.push_chat(reply("a third question"));

    let steps = vec![
        answer("a"),
        answer("b"),
        answer("c"),
        answer("d"),
        answer("e"),
    ];
    let (ui, judged) = run_probe(
        steps,
        &client,
        &config(true),
        &[Category::Ownership, Category::Borrowing],
        None,
    );
    let judged = judged.unwrap();
    assert!(known(&judged).is_empty());

    for category in [Category::Ownership, Category::Borrowing] {
        assert_eq!(
            questions_about(&client, category).len(),
            1 + 1,
            "{category}"
        );
    }
    assert_eq!(ui.asked().len(), 4, "{:?}", ui.asked());
    assert!(!ui.shown().contains(&"a third question".to_owned()));
}

#[test]
fn a_judgement_that_does_not_fit_the_schema_even_after_the_repair_leaves_the_category_unknown() {
    let client = FakeChatClient::new();
    client.push_chat(reply("ownership question"));
    client.push_chat(reply("not json"));
    client.push_chat(reply("{\"elements\": \"still wrong\"}"));
    client.push_chat(reply("borrowing question"));
    client.push_chat(judgement(&all(Category::Borrowing, "known")));

    let steps = vec![answer("a"), answer("b")];
    let (ui, judged) = run_probe(
        steps,
        &client,
        &config(false),
        &[Category::Ownership, Category::Borrowing],
        None,
    );
    let judged = judged.unwrap();
    assert_eq!(known(&judged), elements_of(Category::Borrowing));
    let ownership: Vec<_> = judged
        .iter()
        .filter(|judgement| judgement.element.category() == Category::Ownership)
        .collect();
    assert_eq!(ownership.len(), elements_of(Category::Ownership).len());
    assert!(ownership.iter().all(|judgement| !judgement.known));

    let requests = client.requests();
    assert_eq!(
        requests.len(),
        5,
        "one judgement and its repair: {requests:?}"
    );
    assert_eq!(
        requests[1].response_format,
        Some(ResponseFormat::JsonObject)
    );
    assert!(
        ui.shown()
            .iter()
            .any(|line| line.starts_with("Could not judge the answers about ownership")),
        "{:?}",
        ui.shown()
    );
}

#[test]
fn a_judgement_that_fits_after_the_repair_is_used() {
    let client = FakeChatClient::new();
    client.push_chat(reply("question"));
    client.push_chat(reply("not json"));
    client.push_chat(judgement(&all(Category::Ownership, "known")));

    let (_, judged) = run_probe(
        vec![answer("a")],
        &client,
        &config(false),
        &[Category::Ownership],
        None,
    );
    assert_eq!(known(&judged.unwrap()), elements_of(Category::Ownership));
}

#[test]
fn with_json_schema_a_judgement_that_does_not_fit_leaves_the_category_unknown_without_a_repair() {
    let client = FakeChatClient::new();
    client.push_chat(reply("question"));
    client.push_chat(reply("not json"));

    let (_, judged) = run_probe(
        vec![answer("a")],
        &client,
        &config(true),
        &[Category::Ownership],
        None,
    );
    assert!(known(&judged.unwrap()).is_empty());
    assert_eq!(client.requests().len(), 2);
}

#[test]
fn the_reason_model_makes_the_question_and_the_light_model_judges_with_json_schema() {
    let client = FakeChatClient::new();
    client.push_chat(reply("question"));
    client.push_chat(judgement(&all(Category::Ownership, "known")));

    run_probe(
        vec![answer("a")],
        &client,
        &config(true),
        &[Category::Ownership],
        None,
    )
    .1
    .unwrap();

    let requests = client.requests();
    assert_eq!(requests[0].model, REASON);
    assert_eq!(requests[0].response_format, None);
    assert_eq!(requests[1].model, LIGHT);
    let Some(ResponseFormat::JsonSchema { schema, .. }) = &requests[1].response_format else {
        panic!("{:?}", requests[1].response_format);
    };
    let names = &schema["properties"]["elements"]["items"]["properties"]["element"]["enum"];
    let expected: Vec<_> = elements_of(Category::Ownership)
        .into_iter()
        .map(Element::name)
        .collect();
    assert_eq!(names, &json!(expected));
}

#[test]
fn an_element_the_judgement_leaves_out_or_does_not_list_is_not_known() {
    let client = FakeChatClient::new();
    client.push_chat(reply("question"));
    // `arc` is not an element of ownership, and nothing is said of the others.
    client.push_chat(reply(
        &json!({"elements": [{"element": "arc", "verdict": "known", "reason": "r"}]}).to_string(),
    ));
    client.push_chat(reply("follow-up"));
    client.push_chat(reply(&json!({"elements": []}).to_string()));

    let (ui, judged) = run_probe(
        vec![answer("a"), answer("b")],
        &client,
        &config(false),
        &[Category::Ownership],
        None,
    );
    assert!(known(&judged.unwrap()).is_empty());
    assert!(
        ui.shown().contains(&"follow-up".to_owned()),
        "every element was unclear"
    );
}

#[test]
fn an_api_failure_while_asking_reports_its_cause_and_saves_nothing() {
    let (_dir, path) = no_profile();
    let client = FakeChatClient::new();
    client.push_chat(Err(ChatError::Status {
        status: 401,
        body: "invalid api key".to_owned(),
    }));

    let (ui, outcome) = run_init(
        reports(&[Category::Ownership]),
        &client,
        &config(true),
        &path,
    );
    let error = outcome.unwrap_err();
    assert!(
        error.contains("401") && error.contains("invalid api key"),
        "{error}"
    );
    assert!(error.contains("no profile was saved"), "{error}");
    assert!(!path.exists());
    assert_eq!(ui.asked().len(), Category::ALL.len(), "no answer is asked");
}

#[test]
fn an_api_failure_while_judging_reports_its_cause_and_saves_nothing() {
    let (_dir, path) = no_profile();
    let client = FakeChatClient::new();
    client.push_chat(reply("question"));
    client.push_chat(Err(ChatError::Status {
        status: 429,
        body: "rate limited".to_owned(),
    }));
    let mut steps = reports(&[Category::Ownership, Category::Async]);
    steps.push(answer("a"));

    let (_, outcome) = run_init(steps, &client, &config(true), &path);
    let error = outcome.unwrap_err();
    assert!(
        error.contains("429") && error.contains("rate limited"),
        "{error}"
    );
    assert!(!path.exists());
    assert_eq!(client.requests().len(), 2, "the Q&A stops at the failure");
}

#[test]
fn the_end_of_the_input_at_the_answer_saves_nothing_as_an_interruption() {
    let (_dir, path) = no_profile();
    let client = FakeChatClient::new();
    client.push_chat(reply("question"));
    let mut steps = reports(&[Category::Ownership]);
    steps.push(Step::Eof);

    let (_, outcome) = run_init(steps, &client, &config(true), &path);
    assert_eq!(outcome, Ok(Outcome::Interrupted));
    assert!(!path.exists());
}

#[test]
fn a_failure_to_read_the_answer_reports_its_cause_and_saves_nothing() {
    let (_dir, path) = no_profile();
    let client = FakeChatClient::new();
    client.push_chat(reply("question"));
    // The script answers the question with a confirmation, which the Ui cannot read as a line.
    let mut steps = reports(&[Category::Ownership]);
    steps.push(Step::Confirm(true));

    let (_, outcome) = run_init(steps, &client, &config(true), &path);
    let error = outcome.unwrap_err();
    assert!(error.contains("ui script"), "{error}");
    assert!(error.contains("no profile was saved"), "{error}");
    assert!(!path.exists());
    assert_eq!(client.requests().len(), 1, "nothing is judged");
}

#[test]
fn an_interruption_while_waiting_for_the_model_saves_nothing() {
    let (_dir, path) = no_profile();
    let client = FakeChatClient::new();
    client.push_chat(Err(ChatError::Interrupted));

    let (_, outcome) = run_init(
        reports(&[Category::Ownership]),
        &client,
        &config(true),
        &path,
    );
    assert_eq!(outcome, Ok(Outcome::Interrupted));
    assert!(!path.exists());
}

#[test]
fn interrupting_the_answer_saves_nothing() {
    let (_dir, path) = no_profile();
    let client = FakeChatClient::new();
    client.push_chat(reply("question"));
    let mut steps = reports(&[Category::Ownership]);
    steps.push(Step::Interrupt);

    let (_, outcome) = run_init(steps, &client, &config(true), &path);
    assert_eq!(outcome, Ok(Outcome::Interrupted));
    assert!(!path.exists());
    assert_eq!(client.requests().len(), 1, "nothing is judged");
}

#[test]
fn a_question_the_model_leaves_empty_is_not_asked_and_its_category_stays_unknown() {
    let client = FakeChatClient::new();
    client.push_chat(reply("  "));
    client.push_chat(reply("borrowing question"));
    client.push_chat(judgement(&all(Category::Borrowing, "known")));

    let (ui, judged) = run_probe(
        vec![answer("a")],
        &client,
        &config(true),
        &[Category::Ownership, Category::Borrowing],
        None,
    );
    assert_eq!(known(&judged.unwrap()), elements_of(Category::Borrowing));
    assert_eq!(ui.asked().len(), 1);
}

#[test]
fn the_question_gets_the_places_where_the_reference_repository_uses_the_category_s_elements() {
    let mut elements = BTreeMap::new();
    elements.insert(
        "clone".to_owned(),
        vec![Place {
            path: "src/lib.rs".to_owned(),
            line: 3,
        }],
    );
    elements.insert(
        "arc".to_owned(),
        vec![Place {
            path: "src/arc.rs".to_owned(),
            line: 9,
        }],
    );
    let scan = RepoScan {
        repo: "/repo".to_owned(),
        at: Timestamp::from_unix_seconds(1_791_590_400),
        crates: Vec::new(),
        skipped: Vec::new(),
        elements,
    };
    let client = FakeChatClient::new();
    client.push_chat(reply("question"));
    client.push_chat(judgement(&all(Category::Ownership, "known")));

    run_probe(
        vec![answer("a")],
        &client,
        &config(true),
        &[Category::Ownership],
        Some(&scan),
    )
    .1
    .unwrap();

    let question = user_text(&client.requests()[0]);
    assert!(
        question.contains("`clone`: used at src/lib.rs:3"),
        "{question}"
    );
    assert!(
        question.contains("`to_owned`: not used in the reference repository"),
        "{question}"
    );
    assert!(!question.contains("src/arc.rs"), "{question}");
}

#[test]
fn the_question_and_the_judgement_follow_the_language_of_the_user_s_latest_input() {
    let client = FakeChatClient::new();
    client.push_chat(reply("question 1"));
    client.push_chat(judgement(&all(Category::Ownership, "known")));
    client.push_chat(reply("question 2"));
    client.push_chat(judgement(&all(Category::Borrowing, "known")));

    run_probe(
        vec![answer("String を clone した"), answer("参照を渡した")],
        &client,
        &config(true),
        &[Category::Ownership, Category::Borrowing],
        None,
    )
    .1
    .unwrap();

    let requests = client.requests();
    for request in &requests {
        assert!(system_text(request).contains(LANGUAGE), "{request:?}");
        // Naming the language in the user message led small models to answer in another one.
        assert!(!user_text(request).contains("language"), "{request:?}");
    }
    let second_question = user_text(&requests[2]);
    assert!(
        second_question.contains("The user's latest input was:\nString を clone した"),
        "{second_question}"
    );
}
