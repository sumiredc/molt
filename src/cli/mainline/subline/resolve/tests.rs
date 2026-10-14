use std::fs;
use std::process::ExitCode;

use serde_json::json;
use tempfile::TempDir;

use super::super::super::main_line;
use super::super::super::tests::{input, project, reply};
use super::super::HINT;
use super::*;
use crate::agent::mainline::MainLine;
use crate::agent::tools::Tools;
use crate::config::Config;
use crate::llm::{ChatResponse, FakeChatClient, Message, Router, ToolCall};
use crate::profile::status::Status;
use crate::profile::store::Profile;
use crate::ui::{ScriptedUi, Step};
use crate::workspace::cargo::{CargoOutput, FakeCargo};
use crate::workspace::path::Root;

const WITH_DBG: &str = "fn main() {\n    dbg!(1);\n}\n";
const WITHOUT_DBG: &str = "fn main() {\n    let _ = 1;\n}\n";
const SUB: &str = "molt [sub: dbg_macro]> ";

fn write_main(id: &str, content: &str) -> ChatResponse {
    ChatResponse {
        content: None,
        tool_calls: vec![ToolCall {
            id: id.to_owned(),
            name: "write_file".to_owned(),
            arguments: json!({ "path": "src/main.rs", "content": content }).to_string(),
        }],
    }
}

/// A model that tries `dbg!` twice, which stops the first turn at a boundary, explains it on the
/// side line, then answers with each of `then` in turn.
fn stops_at_a_boundary_then(then: Vec<ChatResponse>) -> FakeChatClient {
    let client = FakeChatClient::new();
    client.push_chat(Ok(write_main("first", WITH_DBG)));
    client.push_chat(Ok(write_main("second", WITH_DBG)));
    client.push_chat(Ok(reply("explanation")));
    for response in then {
        client.push_chat(Ok(response));
    }
    client
}

/// A project with `src/main.rs` and an empty `profile.toml`.
fn project_with_a_profile() -> TempDir {
    let dir = project();
    fs::create_dir(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
    fs::write(dir.path().join("profile.toml"), "").unwrap();
    dir
}

/// Runs the main line on `steps` in `dir`, with the profile read from and saved to its
/// `profile.toml`, and a cargo whose `cargo check` passes.
fn run_in(
    dir: &TempDir,
    client: &FakeChatClient,
    steps: impl IntoIterator<Item = Step>,
) -> (ExitCode, ScriptedUi) {
    let path = dir.path().join("profile.toml");
    let cargo = FakeCargo::new();
    for _ in 0..4 {
        cargo.push(Ok(CargoOutput {
            success: true,
            stdout: String::new(),
            stderr: String::new(),
        }));
    }
    let profile = Profile::load(&path).unwrap();
    let tools = Tools::new(Root::new(dir.path()).unwrap(), &cargo);
    let mut agent = MainLine::new(client, Router::new(&Config::default()), tools, profile)
        .with_profile_path(path);
    let mut ui = ScriptedUi::new(steps);
    let code = main_line(&mut ui, &mut agent);
    (code, ui)
}

fn saved_status(dir: &TempDir) -> Status {
    Profile::load(&dir.path().join("profile.toml"))
        .unwrap()
        .status(&"dbg_macro".parse().unwrap())
}

/// The last user message the `generate` model was sent.
fn last_main_line_input(client: &FakeChatClient) -> String {
    let generate = Config::default().models.generate;
    let request = client
        .requests()
        .into_iter()
        .rfind(|request| request.model == generate)
        .unwrap();
    request
        .messages
        .iter()
        .rev()
        .find_map(|message| match message {
            Message::User { content } => Some(content.clone()),
            _ => None,
        })
        .unwrap()
}

#[test]
fn choosing_1_saves_the_item_as_known_and_the_main_line_goes_on_with_the_unlock_message() {
    let dir = project_with_a_profile();
    let client =
        stops_at_a_boundary_then(vec![write_main("third", WITH_DBG), reply("done with dbg!")]);

    let (code, ui) = run_in(
        &dir,
        &client,
        [
            input("debug 1"),
            input("/choose"),
            Step::Select(1),
            input("/quit"),
        ],
    );

    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(saved_status(&dir), Status::Known);
    assert_eq!(
        fs::read_to_string(dir.path().join("src/main.rs")).unwrap(),
        WITH_DBG
    );
    let shown = ui.shown();
    let at = shown
        .iter()
        .position(|text| text == "Your profile allows dbg_macro now (known).")
        .unwrap_or_else(|| panic!("{shown:?}"));
    assert_eq!(shown[at + 1], "done with dbg!");
    assert!(last_main_line_input(&client).starts_with("BOUNDARY RESOLVED: unlock\n"));
    assert_eq!(
        ui.asked(),
        ["molt> ", SUB, "How do you want to go on?", "molt> "]
    );
}

#[test]
fn choosing_2_saves_the_item_as_seen_and_the_main_line_rewrites_with_the_policy() {
    let dir = project_with_a_profile();
    let client = stops_at_a_boundary_then(vec![
        reply("Print with println! instead."),
        write_main("third", WITHOUT_DBG),
        reply("printed with println!"),
    ]);

    let (code, ui) = run_in(
        &dir,
        &client,
        [
            input("debug 1"),
            input("/choose"),
            Step::Select(2),
            input("/quit"),
        ],
    );

    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(saved_status(&dir), Status::Seen);
    assert!(ui.shown().contains(
        &"Your profile marks dbg_macro as seen, and molt rewrites without them. Policy: Print with println! instead."
            .to_owned()
    ));
    let message = last_main_line_input(&client);
    assert!(
        message.starts_with("BOUNDARY RESOLVED: rewrite\n"),
        "{message}"
    );
    assert!(
        message.contains("Policy: Print with println! instead."),
        "{message}"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("src/main.rs")).unwrap(),
        WITHOUT_DBG
    );
}

#[test]
fn ctrl_c_at_the_choice_stays_on_the_side_line_and_changes_nothing() {
    let dir = project_with_a_profile();
    let client = stops_at_a_boundary_then(Vec::new());

    let (code, ui) = run_in(
        &dir,
        &client,
        [
            input("debug 1"),
            input("/choose"),
            Step::Interrupt,
            input("/abort"),
            input("/quit"),
        ],
    );

    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(
        fs::read_to_string(dir.path().join("profile.toml")).unwrap(),
        ""
    );
    let hint = format!("  {HINT}");
    assert_eq!(
        ui.shown().iter().filter(|text| **text == hint).count(),
        2,
        "{:?}",
        ui.shown()
    );
    assert_eq!(
        ui.asked(),
        ["molt> ", SUB, "How do you want to go on?", SUB, "molt> "]
    );
}

#[test]
fn a_boundary_recurring_after_2_opens_the_side_line_again_recommending_the_others_and_offers_2_choices()
 {
    let dir = project_with_a_profile();
    let client = stops_at_a_boundary_then(vec![
        reply("policy"),
        write_main("third", WITH_DBG),
        reply("explanation again"),
    ]);

    let (code, ui) = run_in(
        &dir,
        &client,
        [
            input("debug 1"),
            input("/choose"),
            Step::Select(2),
            input("/choose"),
            Step::Select(3),
        ],
    );

    assert_eq!(code, ExitCode::FAILURE, "there is no third choice");
    let recurred = format!("  {}", recurred(&["dbg_macro".parse().unwrap()]));
    assert!(ui.shown().contains(&recurred), "{:?}", ui.shown());
    assert_eq!(saved_status(&dir), Status::Seen);
}

#[test]
fn choosing_1_after_a_recurrence_makes_the_item_known() {
    let dir = project_with_a_profile();
    let client = stops_at_a_boundary_then(vec![
        reply("policy"),
        write_main("third", WITH_DBG),
        reply("explanation again"),
        write_main("fourth", WITH_DBG),
        reply("done"),
    ]);

    let (code, _) = run_in(
        &dir,
        &client,
        [
            input("debug 1"),
            input("/choose"),
            Step::Select(2),
            input("/choose"),
            Step::Select(1),
            input("/quit"),
        ],
    );

    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(saved_status(&dir), Status::Known);
}

/// A [`Ui`] that records the items of each selection and chooses the first.
#[derive(Default)]
struct Recording {
    items: Vec<Vec<String>>,
}

impl Ui for Recording {
    fn read_line(&mut self, _prompt: &str) -> Result<crate::ui::Input, UiError> {
        unreachable!("only selections are asked")
    }

    fn show(&mut self, _text: &str) {}

    fn select(&mut self, _prompt: &str, items: &[&str]) -> Result<Option<usize>, UiError> {
        self.items
            .push(items.iter().map(|item| (*item).to_owned()).collect());
        Ok(Some(0))
    }

    fn confirm(&mut self, _prompt: &str) -> Result<Option<bool>, UiError> {
        unreachable!("only selections are asked")
    }
    fn form(
        &mut self,
        _form: &crate::ui::Form,
        _initial: Option<&crate::ui::FormValues>,
    ) -> Result<Option<crate::ui::FormValues>, UiError> {
        unreachable!("only selections are asked")
    }
}

/// The items `choose` offers on a side line at a `dbg_macro` boundary, recurred or not.
fn offered(recurred: bool) -> Vec<String> {
    let dir = project_with_a_profile();
    let client = FakeChatClient::new();
    let cargo = FakeCargo::new();
    let tools = Tools::new(Root::new(dir.path()).unwrap(), &cargo);
    let boundary = crate::agent::mainline::Boundary {
        items: vec!["dbg_macro".parse().unwrap()],
        cause: crate::agent::mainline::BoundaryCause::Requested {
            reason: "it prints".to_owned(),
            proposed_code: None,
        },
    };
    let side = SubLine::open(
        &client,
        Router::new(&Config::default()),
        &tools,
        boundary,
        None,
        &crate::profile::store::Profile::new(),
    )
    .recurred(recurred);
    let mut ui = Recording::default();
    assert_eq!(choose(&mut ui, &side).unwrap(), Some(Choice::Unlock));
    ui.items.pop().unwrap()
}

#[test]
fn choose_offers_1_2_and_3_by_number_in_that_order() {
    assert_eq!(
        offered(false),
        [
            "Understand, then delegate: molt may use dbg_macro from now on",
            "Use a construct I know: molt rewrites without dbg_macro",
            "Write it myself: molt shows where, and checks what I wrote when I type /done",
        ]
    );
}

#[test]
fn choose_offers_1_and_3_only_at_a_boundary_recurring_after_2() {
    assert_eq!(
        offered(true),
        [
            "Understand, then delegate: molt may use dbg_macro from now on",
            "Write it myself: molt shows where, and checks what I wrote when I type /done",
        ]
    );
}

#[test]
fn a_boundary_on_two_categories_asks_on_one_side_line_each_before_the_main_line_goes_on() {
    let dir = project_with_a_profile();
    let client = FakeChatClient::new();
    client.push_chat(Ok(ChatResponse {
        content: None,
        tool_calls: vec![ToolCall {
            id: "boundary".to_owned(),
            name: "request_boundary".to_owned(),
            arguments: json!({ "items": ["arc", "dbg_macro", "mutex"], "reason": "needed" })
                .to_string(),
        }],
    }));
    client.push_chat(Ok(reply("explanation of arc and mutex")));
    client.push_chat(Ok(reply("explanation of dbg!")));
    client.push_chat(Ok(reply("done")));

    let (code, ui) = run_in(
        &dir,
        &client,
        [
            input("share it"),
            input("/choose"),
            Step::Select(1),
            input("/choose"),
            Step::Select(1),
            input("/quit"),
        ],
    );

    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(
        ui.asked(),
        [
            "molt> ",
            "molt [sub: arc, mutex]> ",
            "How do you want to go on?",
            SUB,
            "How do you want to go on?",
            "molt> "
        ]
    );
    let shown = ui.shown();
    let pending = shown
        .iter()
        .position(|text| {
            text == "Chosen for arc, mutex: understand, then delegate. The main line goes on once there is a choice for dbg_macro too."
        })
        .unwrap_or_else(|| panic!("{shown:?}"));
    assert_eq!(
        shown[pending + 1],
        "── side line: dbg_macro ────────────────────────────────────"
    );
    let allowed = shown
        .iter()
        .position(|text| text == "Your profile allows arc, mutex now (known).")
        .unwrap_or_else(|| panic!("{shown:?}"));
    assert_eq!(
        shown[allowed + 1],
        "Your profile allows dbg_macro now (known)."
    );
    assert_eq!(shown[allowed + 2], "done");
    assert_eq!(saved_status(&dir), Status::Known);
    let message = last_main_line_input(&client);
    assert_eq!(
        message.matches("BOUNDARY RESOLVED: unlock").count(),
        2,
        "{message}"
    );
}

#[test]
fn ctrl_c_on_the_side_line_of_the_next_category_comes_back_to_it_and_abort_drops_the_choice_before()
{
    let dir = project_with_a_profile();
    let client = FakeChatClient::new();
    client.push_chat(Ok(ChatResponse {
        content: None,
        tool_calls: vec![ToolCall {
            id: "boundary".to_owned(),
            name: "request_boundary".to_owned(),
            arguments: json!({ "items": ["arc", "dbg_macro"], "reason": "needed" }).to_string(),
        }],
    }));
    client.push_chat(Ok(reply("explanation of arc")));
    client.push_chat(Ok(reply("explanation of dbg!")));

    let (code, ui) = run_in(
        &dir,
        &client,
        [
            input("share it"),
            input("/choose"),
            Step::Select(1),
            Step::Interrupt,
            input("/side"),
            input("/abort"),
            input("/quit"),
        ],
    );

    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(
        ui.asked(),
        [
            "molt> ",
            "molt [sub: arc]> ",
            "How do you want to go on?",
            SUB,
            "molt> ",
            SUB,
            "molt> "
        ]
    );
    assert!(ui.shown().iter().any(|text| text == "Request withdrawn."));
    assert_eq!(client.requests().len(), 3, "no request after the choice");
    assert_eq!(
        fs::read_to_string(dir.path().join("profile.toml")).unwrap(),
        ""
    );
}
