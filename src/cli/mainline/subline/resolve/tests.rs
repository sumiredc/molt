use std::fs;
use std::process::ExitCode;

use serde_json::json;
use tempfile::TempDir;

use super::super::super::main_line;
use super::super::super::tests::{input, project, reply};
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
        [input("debug 1"), Step::Select(1), input("/quit")],
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
        .position(|text| text == "Your profile has dbg_macro as known now.")
        .unwrap_or_else(|| panic!("{shown:?}"));
    assert_eq!(
        shown[at + 1..at + 3],
        ["● write_file src/main.rs", "done with dbg!"]
    );
    assert!(last_main_line_input(&client).starts_with("BOUNDARY RESOLVED: unlock\n"));
    assert_eq!(
        ui.asked(),
        ["molt> ", "How do you want to go on?", "molt> "]
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
        [input("debug 1"), Step::Select(2), input("/quit")],
    );

    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(saved_status(&dir), Status::Seen);
    assert!(ui.shown().contains(
        &"Your profile marks dbg_macro as seen, and molt writes it without them. Policy: Print with println! instead."
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
fn ctrl_c_at_the_choices_pauses_the_main_line_at_the_boundary_and_changes_nothing() {
    let dir = project_with_a_profile();
    let client = stops_at_a_boundary_then(Vec::new());

    let (code, ui) = run_in(
        &dir,
        &client,
        [
            input("debug 1"),
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
    assert!(
        ui.shown()
            .iter()
            .any(|text| text.starts_with("Paused at the boundary.")),
        "{:?}",
        ui.shown()
    );
    assert_eq!(ui.asked(), ["molt> ", SUB, "molt> ", "molt> "]);
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
        [input("debug 1"), Step::Select(2), Step::Select(3)],
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
            Step::Select(2),
            Step::Select(1),
            input("/quit"),
        ],
    );

    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(saved_status(&dir), Status::Known);
}

/// The choices offered, numbered, on a side line at a `dbg_macro` boundary, recurred or not.
fn listed(recurred: bool) -> Vec<(usize, String)> {
    let dir = project_with_a_profile();
    let client = FakeChatClient::new();
    let cargo = FakeCargo::new();
    let tools = Tools::new(Root::new(dir.path()).unwrap(), &cargo);
    let boundary = crate::agent::mainline::Boundary {
        items: vec!["dbg_macro".parse().unwrap()],
        step: crate::agent::mainline::Step::new(
            "src/main.rs".to_owned(),
            None,
            "fn main() {\n    dbg!(1);\n}\n".to_owned(),
        ),
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
    offered(&side)
        .into_iter()
        .map(|(choice, label)| (choice.number(), label))
        .collect()
}

#[test]
fn the_choices_are_1_2_3_and_4_in_that_order() {
    assert_eq!(
        listed(false),
        [
            (1, "Understood: the step stays, and molt uses dbg_macro from now on".to_owned()),
            (2, "Write it without them: molt takes the step back and writes it without dbg_macro".to_owned()),
            (3, "Write it myself: molt takes the step back, shows where, and checks what I wrote when I type /done".to_owned()),
            (4, "Next: the step stays, and molt explains dbg_macro again when it uses them".to_owned()),
        ]
    );
}

#[test]
fn at_a_boundary_recurring_after_2_there_is_no_2_and_the_others_keep_their_numbers() {
    assert_eq!(
        listed(true),
        [
            (1, "Understood: the step stays, and molt uses dbg_macro from now on".to_owned()),
            (3, "Write it myself: molt takes the step back, shows where, and checks what I wrote when I type /done".to_owned()),
            (4, "Next: the step stays, and molt explains dbg_macro again when it uses them".to_owned()),
        ]
    );
}

#[test]
fn a_step_with_items_of_two_categories_gets_one_side_line_and_one_choice_for_all() {
    let dir = project_with_a_profile();
    let client = FakeChatClient::new();
    client.push_chat(Ok(write_main(
        "step",
        "use std::sync::Arc;\n\nfn main() {\n    dbg!(Arc::new(1));\n}\n",
    )));
    client.push_chat(Ok(reply("explanation of arc and dbg!")));
    client.push_chat(Ok(reply("done")));

    let (code, ui) = run_in(
        &dir,
        &client,
        [input("share it"), Step::Select(1), input("/quit")],
    );

    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(
        ui.asked(),
        ["molt> ", "How do you want to go on?", "molt> "]
    );
    assert!(
        ui.shown()
            .contains(&"Your profile has arc, dbg_macro as known now.".to_owned()),
        "{:?}",
        ui.shown()
    );
    assert_eq!(saved_status(&dir), Status::Known);
    let message = last_main_line_input(&client);
    assert_eq!(message.matches("BOUNDARY RESOLVED: unlock").count(), 1);
}

#[test]
fn choosing_4_keeps_the_step_marks_the_item_seen_and_the_main_line_goes_on() {
    let dir = project_with_a_profile();
    let client = stops_at_a_boundary_then(vec![reply("done with dbg!")]);

    let (code, ui) = run_in(
        &dir,
        &client,
        [input("debug 1"), Step::Select(4), input("/quit")],
    );

    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(saved_status(&dir), Status::Seen);
    assert_eq!(
        fs::read_to_string(dir.path().join("src/main.rs")).unwrap(),
        WITH_DBG,
        "the step stays"
    );
    assert!(
        ui.shown().contains(
            &"Your profile marks dbg_macro as seen; molt explains them again when it uses them."
                .to_owned()
        )
    );
    assert!(last_main_line_input(&client).starts_with("BOUNDARY RESOLVED: next\n"));
}
