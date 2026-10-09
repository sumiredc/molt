use std::cell::RefCell;
use std::fs;

use serde_json::{Value, json};
use tempfile::TempDir;

use super::super::{Message, Router, Tools};
use super::*;
use crate::agent::tools::ApprovalRequest;
use crate::config::Config;
use crate::llm::{ChatResponse, FakeChatClient};
use crate::profile::status::{ItemKey, Provenance, Status};
use crate::profile::store::{Profile, Timestamp};
use crate::workspace::Approval;
use crate::workspace::cargo::{
    CargoCall, CargoError, CargoOutput, CargoRunner, FakeCargo, Subcommand,
};
use crate::workspace::path::Root;

/// Answers every approval with `answer`, and records what was asked.
struct Answer {
    answer: bool,
    asked: Vec<ApprovalRequest>,
}

impl Answer {
    fn yes() -> Self {
        Self {
            answer: true,
            asked: Vec::new(),
        }
    }

    fn no() -> Self {
        Self {
            answer: false,
            asked: Vec::new(),
        }
    }
}

impl Approver for Answer {
    fn confirm(&mut self, request: &ApprovalRequest) -> bool {
        self.asked.push(request.clone());
        self.answer
    }
}

const MANIFEST_TEXT: &str = "[package]\nname = \"demo\"\n\n[dependencies]\n";

fn project() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("Cargo.toml"), MANIFEST_TEXT).unwrap();
    fs::create_dir(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
    dir
}

fn main_line<'a>(
    client: &'a FakeChatClient,
    cargo: &'a dyn CargoRunner,
    dir: &TempDir,
    profile: Profile,
) -> MainLine<'a> {
    let tools = Tools::new(Root::new(dir.path()).unwrap(), cargo);
    MainLine::new(client, Router::new(&Config::default()), tools, profile)
}

fn call(id: &str, name: &str, arguments: Value) -> ChatResponse {
    ChatResponse {
        content: None,
        tool_calls: vec![ToolCall {
            id: id.to_owned(),
            name: name.to_owned(),
            arguments: arguments.to_string(),
        }],
    }
}

fn write_file(id: &str, path: &str, content: &str) -> ChatResponse {
    call(
        id,
        "write_file",
        json!({ "path": path, "content": content }),
    )
}

fn finish(id: &str) -> ChatResponse {
    call(id, "finish", json!({ "summary": "done" }))
}

fn reply(text: &str) -> ChatResponse {
    ChatResponse {
        content: Some(text.to_owned()),
        tool_calls: Vec::new(),
    }
}

fn output(success: bool, stderr: &str) -> CargoOutput {
    CargoOutput {
        success,
        stdout: String::new(),
        stderr: stderr.to_owned(),
    }
}

fn passed() -> Result<CargoOutput, CargoError> {
    Ok(output(true, ""))
}

fn failed(stderr: &str) -> Result<CargoOutput, CargoError> {
    Ok(output(false, stderr))
}

fn checks(count: usize) -> Vec<CargoCall> {
    vec![CargoCall::Run(Subcommand::Check); count]
}

fn known_crate(name: &str) -> Profile {
    let mut profile = Profile::new();
    profile
        .transition(
            ItemKey::Crate(name.to_owned()),
            Status::Known,
            Provenance::Manual,
            Timestamp::from_unix_seconds(0),
            None,
        )
        .unwrap();
    profile
}

const CLEAN: &str = "fn main() {\n    println!(\"{}\", 1);\n}\n";
const USES_SERDE_JSON: &str = "fn main() {\n    let _ = serde_json::to_string(&1);\n}\n";
/// Does not parse: a `let` without a pattern.
const BROKEN: &str = "fn main() {\n    let = 1;\n}\n";
const MISMATCH: &str = "error[E0308]: mismatched types";

/// The tool results molt put in the message list in its own name, after a failed check.
fn molt_check_results(main_line: &MainLine) -> Vec<String> {
    let ids: Vec<String> = main_line
        .messages()
        .iter()
        .filter_map(|message| match message {
            Message::Assistant { tool_calls, .. } => Some(tool_calls),
            _ => None,
        })
        .flatten()
        .filter(|call| call.id.starts_with("molt_check_"))
        .map(|call| call.id.clone())
        .collect();
    ids.iter()
        .map(|id| {
            main_line
                .messages()
                .iter()
                .find_map(|message| match message {
                    Message::Tool {
                        tool_call_id,
                        content,
                    } if tool_call_id == id => Some(content.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("no tool result for {id}"))
        })
        .collect()
}

#[test]
fn a_turn_that_wrote_ends_with_cargo_check_though_the_model_did_not_call_it() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    client.push_chat(Ok(write_file("w", "src/main.rs", CLEAN)));
    client.push_chat(Ok(reply("done")));
    cargo.push(passed());
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

    let turn = main_line.turn("print 1", &mut Answer::no());

    assert_eq!(cargo.calls(), checks(1));
    assert!(
        matches!(turn.check, Some(Ok(ref o)) if o.success),
        "{:?}",
        turn.check
    );
    assert!(matches!(turn.end, TurnEnd::Finished), "{:?}", turn.end);
    assert_eq!(turn.compile_fixes, 0);
}

#[test]
fn a_turn_that_wrote_nothing_runs_no_cargo_check() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    client.push_chat(Ok(call("r", "read_file", json!({ "path": "src/main.rs" }))));
    client.push_chat(Ok(reply("it is empty")));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

    let turn = main_line.turn("read main", &mut Answer::no());

    assert!(cargo.calls().is_empty());
    assert!(turn.check.is_none());
}

#[test]
fn a_rejected_write_is_not_a_write_and_runs_no_cargo_check() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    client.push_chat(Ok(write_file(
        "w",
        "src/main.rs",
        "fn main() {\n    dbg!(1);\n}\n",
    )));
    client.push_chat(Ok(reply("cannot")));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

    let turn = main_line.turn("debug 1", &mut Answer::no());

    assert!(cargo.calls().is_empty());
    assert!(turn.check.is_none());
}

#[test]
fn a_failed_check_goes_back_to_the_model_and_the_turn_goes_on_until_it_passes() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    client.push_chat(Ok(write_file("w1", "src/main.rs", CLEAN)));
    client.push_chat(Ok(finish("f1")));
    client.push_chat(Ok(write_file("w2", "src/main.rs", CLEAN)));
    client.push_chat(Ok(finish("f2")));
    cargo.push(failed(MISMATCH));
    cargo.push(passed());
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

    let turn = main_line.turn("print 1", &mut Answer::no());

    assert!(matches!(turn.end, TurnEnd::Finished), "{:?}", turn.end);
    assert_eq!(cargo.calls(), checks(2));
    assert_eq!(turn.compile_fixes, 1);
    assert!(matches!(turn.check, Some(Ok(ref o)) if o.success));
    let results = molt_check_results(&main_line);
    assert_eq!(results.len(), 1);
    assert!(results[0].contains(MISMATCH), "{}", results[0]);
    assert!(
        results[0].contains("the turn is not over"),
        "{}",
        results[0]
    );
    // The failure is not a user message: the user's input stays the latest one.
    let sent = &client.requests()[2].messages;
    let users: Vec<_> = sent
        .iter()
        .filter(|m| matches!(m, Message::User { .. }))
        .collect();
    assert_eq!(
        users,
        [&Message::User {
            content: "print 1".to_owned()
        }]
    );
    assert!(
        matches!(sent.last(), Some(Message::Tool { content, .. }) if content.contains(MISMATCH)),
        "{sent:?}"
    );
    assert_eq!(
        turn.tool_calls,
        ["write_file", "finish", "write_file", "finish"],
        "molt's cargo check is not a tool call of the model"
    );
}

#[test]
fn a_failed_check_after_a_reply_without_tool_calls_goes_back_to_the_model_too() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    client.push_chat(Ok(write_file("w", "src/main.rs", CLEAN)));
    client.push_chat(Ok(reply("done")));
    client.push_chat(Ok(reply("I could not fix it")));
    cargo.push(failed(MISMATCH));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

    let turn = main_line.turn("print 1", &mut Answer::no());

    assert_eq!(client.requests().len(), 3);
    assert_eq!(turn.compile_fixes, 1);
    assert_eq!(turn.reply.as_deref(), Some("I could not fix it"));
}

#[test]
fn the_fourth_failed_check_stops_the_turn_without_asking_for_another_fix() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    for i in 0..=COMPILE_FIX_BUDGET {
        client.push_chat(Ok(write_file(&format!("w{i}"), "src/main.rs", CLEAN)));
        client.push_chat(Ok(finish(&format!("f{i}"))));
        cargo.push(failed(MISMATCH));
    }
    client.push_chat(Ok(reply("never sent")));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

    let turn = main_line.turn("print 1", &mut Answer::no());

    assert!(
        matches!(turn.end, TurnEnd::CompileFixesExhausted),
        "{:?}",
        turn.end
    );
    assert_eq!(turn.compile_fixes, COMPILE_FIX_BUDGET);
    assert_eq!(cargo.calls(), checks(COMPILE_FIX_BUDGET + 1));
    assert_eq!(client.requests().len(), 2 * (COMPILE_FIX_BUDGET + 1));
    assert_eq!(molt_check_results(&main_line).len(), COMPILE_FIX_BUDGET);
    assert!(matches!(turn.check, Some(Ok(ref o)) if !o.success));
}

#[test]
fn the_third_failed_check_still_gets_a_fix() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    for i in 0..COMPILE_FIX_BUDGET {
        client.push_chat(Ok(write_file(&format!("w{i}"), "src/main.rs", CLEAN)));
        client.push_chat(Ok(finish(&format!("f{i}"))));
        cargo.push(failed(MISMATCH));
    }
    client.push_chat(Ok(write_file("last", "src/main.rs", CLEAN)));
    client.push_chat(Ok(finish("done")));
    cargo.push(passed());
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

    let turn = main_line.turn("print 1", &mut Answer::no());

    assert!(matches!(turn.end, TurnEnd::Finished), "{:?}", turn.end);
    assert_eq!(turn.compile_fixes, COMPILE_FIX_BUDGET);
}

#[test]
fn the_fourth_parse_error_stops_the_turn() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    for i in 0..=COMPILE_FIX_BUDGET {
        client.push_chat(Ok(write_file(&format!("p{i}"), "src/main.rs", BROKEN)));
    }
    client.push_chat(Ok(reply("never sent")));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

    let turn = main_line.turn("print 1", &mut Answer::no());

    assert!(
        matches!(turn.end, TurnEnd::CompileFixesExhausted),
        "{:?}",
        turn.end
    );
    assert_eq!(turn.compile_fixes, COMPILE_FIX_BUDGET);
    assert_eq!(client.requests().len(), COMPILE_FIX_BUDGET + 1);
    assert!(cargo.calls().is_empty(), "nothing was written");
}

#[test]
fn parse_errors_and_failed_checks_count_together_toward_the_budget() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    client.push_chat(Ok(write_file("p1", "src/main.rs", BROKEN)));
    client.push_chat(Ok(write_file("p2", "src/main.rs", BROKEN)));
    client.push_chat(Ok(write_file("w", "src/main.rs", CLEAN)));
    client.push_chat(Ok(finish("f")));
    cargo.push(failed(MISMATCH));
    client.push_chat(Ok(write_file("p3", "src/main.rs", BROKEN)));
    client.push_chat(Ok(reply("never sent")));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

    let turn = main_line.turn("print 1", &mut Answer::no());

    assert!(
        matches!(turn.end, TurnEnd::CompileFixesExhausted),
        "{:?}",
        turn.end
    );
    assert_eq!(client.requests().len(), 5);
    assert_eq!(
        cargo.calls(),
        checks(1),
        "nothing was written since the last check"
    );
}

#[test]
fn a_turn_stopped_by_a_parse_error_after_a_write_ends_with_a_check_and_no_fix() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    client.push_chat(Ok(write_file("w", "src/lib.rs", "pub fn f() {}\n")));
    for i in 0..=COMPILE_FIX_BUDGET {
        client.push_chat(Ok(write_file(&format!("p{i}"), "src/main.rs", BROKEN)));
    }
    cargo.push(failed(MISMATCH));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

    let turn = main_line.turn("add f", &mut Answer::no());

    assert!(matches!(turn.end, TurnEnd::CompileFixesExhausted));
    assert_eq!(cargo.calls(), checks(1));
    assert!(matches!(turn.check, Some(Ok(ref o)) if !o.success));
    assert!(molt_check_results(&main_line).is_empty());
}

#[test]
fn after_a_fix_that_wrote_nothing_the_check_is_not_run_again() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    client.push_chat(Ok(write_file("w", "src/main.rs", CLEAN)));
    client.push_chat(Ok(finish("f1")));
    client.push_chat(Ok(finish("f2")));
    cargo.push(failed(MISMATCH));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

    let turn = main_line.turn("print 1", &mut Answer::no());

    assert!(matches!(turn.end, TurnEnd::Finished), "{:?}", turn.end);
    assert_eq!(cargo.calls(), checks(1));
    assert!(matches!(turn.check, Some(Ok(ref o)) if !o.success));
}

#[test]
fn a_check_cargo_cannot_run_is_not_a_compile_fix() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    client.push_chat(Ok(write_file("w", "src/main.rs", CLEAN)));
    client.push_chat(Ok(reply("done")));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

    let turn = main_line.turn("print 1", &mut Answer::no());

    assert!(matches!(turn.end, TurnEnd::Finished), "{:?}", turn.end);
    assert_eq!(turn.compile_fixes, 0);
    assert!(matches!(turn.check, Some(Err(CargoError::FakeExhausted))));
    assert_eq!(client.requests().len(), 2);
}

#[test]
fn a_turn_stopped_by_an_api_error_after_a_write_ends_with_a_check_and_no_fix() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    client.push_chat(Ok(write_file("w", "src/main.rs", CLEAN)));
    client.push_chat(Err(crate::llm::ChatError::Status {
        status: 500,
        body: "down".to_owned(),
    }));
    cargo.push(failed(MISMATCH));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

    let turn = main_line.turn("print 1", &mut Answer::no());

    assert!(matches!(turn.end, TurnEnd::ApiFailed(_)), "{:?}", turn.end);
    assert_eq!(cargo.calls(), checks(1));
    assert_eq!(client.requests().len(), 2);
}

#[test]
fn at_a_boundary_no_check_runs() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    let with_dbg = "fn main() {\n    dbg!(1);\n}\n";
    client.push_chat(Ok(write_file("w", "src/lib.rs", "pub fn f() {}\n")));
    client.push_chat(Ok(write_file("d1", "src/main.rs", with_dbg)));
    client.push_chat(Ok(write_file("d2", "src/main.rs", with_dbg)));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

    let turn = main_line.turn("debug 1", &mut Answer::no());

    assert!(matches!(turn.end, TurnEnd::Boundary(_)), "{:?}", turn.end);
    assert!(cargo.calls().is_empty());
    assert!(turn.check.is_none());
}

#[test]
fn molt_checks_have_ids_of_their_own_across_turns() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    for _ in 0..2 {
        client.push_chat(Ok(write_file("w", "src/main.rs", CLEAN)));
        client.push_chat(Ok(reply("done")));
        client.push_chat(Ok(reply("gave up")));
        cargo.push(failed(MISMATCH));
    }
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

    main_line.turn("print 1", &mut Answer::no());
    main_line.turn("print 2", &mut Answer::no());

    assert_eq!(molt_check_results(&main_line).len(), 2);
    let ids: Vec<&str> = main_line
        .messages()
        .iter()
        .filter_map(|m| match m {
            Message::Tool { tool_call_id, .. } if tool_call_id.starts_with("molt_check_") => {
                Some(tool_call_id.as_str())
            }
            _ => None,
        })
        .collect();
    assert_eq!(ids, ["molt_check_1", "molt_check_2"]);
}

#[test]
fn the_output_returned_to_the_model_is_cut_at_its_line_limit() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    client.push_chat(Ok(write_file("w", "src/main.rs", CLEAN)));
    client.push_chat(Ok(reply("done")));
    client.push_chat(Ok(reply("gave up")));
    let long: String = (1..=100).map(|n| format!("error line {n}\n")).collect();
    cargo.push(failed(&long));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

    main_line.turn("print 1", &mut Answer::no());

    let result = &molt_check_results(&main_line)[0];
    assert!(result.contains("error line 80\n"), "{result}");
    assert!(!result.contains("error line 81\n"), "{result}");
    assert!(result.contains("... 20 more lines"), "{result}");
}

#[test]
fn an_approved_known_crate_missing_from_cargo_toml_is_added_before_the_check() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    client.push_chat(Ok(write_file("w", "src/main.rs", USES_SERDE_JSON)));
    client.push_chat(Ok(reply("done")));
    cargo.push(passed());
    cargo.push(passed());
    let mut main_line = main_line(&client, &cargo, &dir, known_crate("serde_json"));
    let mut approver = Answer::yes();

    let turn = main_line.turn("serialize 1", &mut approver);

    let serde_json = CrateName::new("serde_json").unwrap();
    assert_eq!(
        cargo.calls(),
        [
            CargoCall::Add(serde_json.clone()),
            CargoCall::Run(Subcommand::Check)
        ]
    );
    assert_eq!(approver.asked, [ApprovalRequest::CargoAdd(serde_json)]);
    assert_eq!(turn.crate_adds.len(), 1);
    assert!(matches!(turn.crate_adds[0].outcome, AddOutcome::Ran(_)));
}

#[test]
fn a_known_crate_the_user_does_not_approve_is_not_added_and_the_model_is_told() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    client.push_chat(Ok(write_file("w", "src/main.rs", USES_SERDE_JSON)));
    client.push_chat(Ok(reply("done")));
    client.push_chat(Ok(write_file("w2", "src/main.rs", USES_SERDE_JSON)));
    client.push_chat(Ok(reply("still needs it")));
    cargo.push(failed("error[E0433]: unresolved crate `serde_json`"));
    cargo.push(failed("error[E0433]: unresolved crate `serde_json`"));
    let mut main_line = main_line(&client, &cargo, &dir, known_crate("serde_json"));
    let mut approver = Answer::no();

    let turn = main_line.turn("serialize 1", &mut approver);

    assert_eq!(cargo.calls(), checks(2), "no add");
    assert_eq!(approver.asked.len(), 1, "not asked again in the same turn");
    assert!(matches!(
        turn.crate_adds[0].outcome,
        AddOutcome::NotApproved
    ));
    let results = molt_check_results(&main_line);
    for result in &results {
        assert!(
            result.contains("the user did not approve `cargo add serde_json`"),
            "{result}"
        );
    }
}

#[test]
fn a_crate_the_model_was_refused_is_not_asked_about_again_at_the_end_of_the_turn() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    client.push_chat(Ok(call(
        "a",
        "run_cargo",
        json!({ "subcommand": "add", "args": ["serde-json"] }),
    )));
    client.push_chat(Ok(write_file("w", "src/main.rs", USES_SERDE_JSON)));
    client.push_chat(Ok(finish("f")));
    cargo.push(passed());
    let mut main_line = main_line(&client, &cargo, &dir, known_crate("serde_json"));
    let mut approver = Answer::no();

    main_line.turn("serialize 1", &mut approver);

    assert_eq!(approver.asked.len(), 1, "{:?}", approver.asked);
    assert_eq!(cargo.calls(), checks(1));
}

#[test]
fn a_crate_the_model_failed_to_add_is_not_asked_about_again_at_the_end_of_the_turn() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    client.push_chat(Ok(call(
        "a",
        "run_cargo",
        json!({ "subcommand": "add", "args": ["serde_json"] }),
    )));
    client.push_chat(Ok(write_file("w", "src/main.rs", USES_SERDE_JSON)));
    client.push_chat(Ok(finish("f")));
    cargo.push(failed("error: no network"));
    cargo.push(passed());
    let mut main_line = main_line(&client, &cargo, &dir, known_crate("serde_json"));
    let mut approver = Answer::yes();

    main_line.turn("serialize 1", &mut approver);

    assert_eq!(approver.asked.len(), 1, "{:?}", approver.asked);
    assert_eq!(
        cargo.calls(),
        [
            CargoCall::Add(CrateName::new("serde_json").unwrap()),
            CargoCall::Run(Subcommand::Check)
        ]
    );
}

#[test]
fn a_crate_cargo_toml_declares_is_not_asked_about() {
    let dir = project();
    fs::write(
        dir.path().join("Cargo.toml"),
        format!("{MANIFEST_TEXT}serde_json = \"1\"\n"),
    )
    .unwrap();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    client.push_chat(Ok(write_file("w", "src/main.rs", USES_SERDE_JSON)));
    client.push_chat(Ok(reply("done")));
    cargo.push(passed());
    let mut main_line = main_line(&client, &cargo, &dir, known_crate("serde_json"));
    let mut approver = Answer::yes();

    main_line.turn("serialize 1", &mut approver);

    assert!(approver.asked.is_empty());
    assert_eq!(cargo.calls(), checks(1));
}

#[test]
fn the_changes_are_each_written_file_from_its_content_before_the_turn() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    client.push_chat(Ok(write_file(
        "w1",
        "src/main.rs",
        BROKEN.replace("let =", "let x =").as_str(),
    )));
    client.push_chat(Ok(write_file("w2", "src/lib.rs", "pub fn f() {}\n")));
    client.push_chat(Ok(write_file("w3", "src/main.rs", CLEAN)));
    client.push_chat(Ok(reply("done")));
    cargo.push(passed());
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

    let turn = main_line.turn("print 1", &mut Answer::no());

    assert_eq!(
        turn.changes,
        [
            Change {
                path: "src/main.rs".to_owned(),
                before: Some("fn main() {}\n".to_owned()),
                after: CLEAN.to_owned(),
            },
            Change {
                path: "src/lib.rs".to_owned(),
                before: None,
                after: "pub fn f() {}\n".to_owned(),
            },
        ]
    );
}

/// A cargo whose `add` adds the crate to `Cargo.toml`, and whose `fmt` rewrites `src/main.rs`,
/// as the real ones do. Every call succeeds.
struct EditingCargo {
    dir: std::path::PathBuf,
    calls: RefCell<Vec<CargoCall>>,
}

impl CargoRunner for EditingCargo {
    fn run(&self, subcommand: Subcommand) -> Result<CargoOutput, CargoError> {
        self.calls.borrow_mut().push(CargoCall::Run(subcommand));
        if subcommand == Subcommand::Fmt {
            fs::write(self.dir.join("src/main.rs"), "// formatted\n").unwrap();
        }
        passed()
    }

    fn add(&self, crate_name: &CrateName, _approval: Approval) -> Result<CargoOutput, CargoError> {
        self.calls
            .borrow_mut()
            .push(CargoCall::Add(crate_name.clone()));
        let manifest = self.dir.join("Cargo.toml");
        let mut text = fs::read_to_string(&manifest).unwrap();
        text.push_str(&format!("{} = \"1\"\n", crate_name.as_str()));
        fs::write(manifest, text).unwrap();
        passed()
    }
}

#[test]
fn cargo_toml_changed_by_cargo_add_is_among_the_changes() {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = EditingCargo {
        dir: dir.path().to_path_buf(),
        calls: RefCell::new(Vec::new()),
    };
    client.push_chat(Ok(write_file("w", "src/main.rs", USES_SERDE_JSON)));
    client.push_chat(Ok(reply("done")));
    let mut main_line = main_line(&client, &cargo, &dir, known_crate("serde_json"));

    let turn = main_line.turn("serialize 1", &mut Answer::yes());

    let manifest = turn
        .changes
        .iter()
        .find(|c| c.path == "Cargo.toml")
        .expect("Cargo.toml is among the changes");
    assert_eq!(manifest.before.as_deref(), Some(MANIFEST_TEXT));
    assert!(
        manifest.after.ends_with("serde_json = \"1\"\n"),
        "{manifest:?}"
    );
}

#[test]
fn a_change_shows_the_file_as_it_is_at_the_end_of_the_turn() {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = EditingCargo {
        dir: dir.path().to_path_buf(),
        calls: RefCell::new(Vec::new()),
    };
    client.push_chat(Ok(write_file("w", "src/main.rs", CLEAN)));
    client.push_chat(Ok(call("fmt", "run_cargo", json!({ "subcommand": "fmt" }))));
    client.push_chat(Ok(reply("done")));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

    let turn = main_line.turn("print 1", &mut Answer::no());

    assert_eq!(turn.changes[0].after, "// formatted\n");
}

#[test]
fn a_rejection_before_a_finish_still_counts_in_the_fix_after_a_failed_check() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    let with_dbg = "fn main() {\n    dbg!(1);\n}\n";
    let tool_call = |id: &str, name: &str, arguments: Value| ToolCall {
        id: id.to_owned(),
        name: name.to_owned(),
        arguments: arguments.to_string(),
    };
    client.push_chat(Ok(ChatResponse {
        content: None,
        tool_calls: vec![
            tool_call(
                "w",
                "write_file",
                json!({ "path": "src/lib.rs", "content": "pub fn f() {}\n" }),
            ),
            tool_call(
                "d1",
                "write_file",
                json!({ "path": "src/main.rs", "content": with_dbg }),
            ),
            tool_call("f", "finish", json!({})),
        ],
    }));
    cargo.push(failed(MISMATCH));
    client.push_chat(Ok(write_file("d2", "src/main.rs", with_dbg)));
    client.push_chat(Ok(reply("never sent")));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

    let turn = main_line.turn("debug 1", &mut Answer::no());

    assert!(matches!(turn.end, TurnEnd::Boundary(_)), "{:?}", turn.end);
    assert_eq!(client.requests().len(), 2);
}

#[test]
fn a_crate_the_model_added_is_checked_at_the_end_of_the_turn_and_shown_among_the_changes() {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = EditingCargo {
        dir: dir.path().to_path_buf(),
        calls: RefCell::new(Vec::new()),
    };
    client.push_chat(Ok(call(
        "a",
        "run_cargo",
        json!({ "subcommand": "add", "args": ["serde_json"] }),
    )));
    client.push_chat(Ok(reply("added")));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

    let turn = main_line.turn("add serde_json", &mut Answer::yes());

    assert_eq!(
        *cargo.calls.borrow(),
        [
            CargoCall::Add(CrateName::new("serde_json").unwrap()),
            CargoCall::Run(Subcommand::Check)
        ]
    );
    assert_eq!(turn.changes.len(), 1);
    assert_eq!(turn.changes[0].path, "Cargo.toml");
}

#[test]
fn a_crate_cargo_add_failed_on_is_not_asked_about_again_in_the_turn() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    client.push_chat(Ok(write_file("w", "src/main.rs", USES_SERDE_JSON)));
    client.push_chat(Ok(reply("done")));
    client.push_chat(Ok(write_file("w2", "src/main.rs", USES_SERDE_JSON)));
    client.push_chat(Ok(reply("still needs it")));
    cargo.push(failed("error: no network"));
    cargo.push(failed(MISMATCH));
    cargo.push(failed(MISMATCH));
    let mut main_line = main_line(&client, &cargo, &dir, known_crate("serde_json"));
    let mut approver = Answer::yes();

    main_line.turn("serialize 1", &mut approver);

    assert_eq!(approver.asked.len(), 1, "{:?}", approver.asked);
    let results = molt_check_results(&main_line);
    assert_eq!(results.len(), 2);
    for result in &results {
        assert!(
            result.contains("`cargo add serde_json` failed: error: no network"),
            "{result}"
        );
    }
}

#[test]
fn a_file_written_under_two_spellings_is_one_change() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    client.push_chat(Ok(write_file("w1", "src/main.rs", CLEAN)));
    client.push_chat(Ok(write_file(
        "w2",
        "./src/main.rs",
        "fn main() {\n    println!(\"{}\", 2);\n}\n",
    )));
    client.push_chat(Ok(reply("done")));
    cargo.push(passed());
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

    let turn = main_line.turn("print 2", &mut Answer::no());

    assert_eq!(turn.changes.len(), 1, "{:?}", turn.changes);
    assert_eq!(turn.changes[0].before.as_deref(), Some("fn main() {}\n"));
}

#[test]
fn the_reply_before_a_failed_check_is_not_the_reply_of_the_turn() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    client.push_chat(Ok(write_file("w1", "src/main.rs", CLEAN)));
    client.push_chat(Ok(reply("Printed 1.")));
    client.push_chat(Ok(write_file("w2", "src/main.rs", CLEAN)));
    client.push_chat(Ok(call("f", "finish", json!({}))));
    cargo.push(failed(MISMATCH));
    cargo.push(passed());
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

    let turn = main_line.turn("print 1", &mut Answer::no());

    assert_eq!(turn.reply, None);
}

/// A model that writes `src/lib.rs`, then tries `dbg!` twice, which stops the turn at a
/// boundary.
fn wrote_then_boundary(client: &FakeChatClient) {
    let with_dbg = "fn main() {\n    dbg!(1);\n}\n";
    client.push_chat(Ok(write_file("w", "src/lib.rs", "pub fn f() {}\n")));
    client.push_chat(Ok(write_file("d1", "src/main.rs", with_dbg)));
    client.push_chat(Ok(write_file("d2", "src/main.rs", with_dbg)));
}

#[test]
fn withdrawing_a_request_held_at_a_boundary_checks_what_was_written_before_it() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    wrote_then_boundary(&client);
    cargo.push(failed(MISMATCH));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());
    let turn = main_line.turn("debug 1", &mut Answer::no());
    assert!(matches!(turn.end, TurnEnd::Boundary(_)), "{:?}", turn.end);
    let messages_before = main_line.messages().len();

    let withdrawal = main_line.withdraw();

    assert_eq!(cargo.calls(), checks(1));
    assert!(matches!(withdrawal.check, Some(Ok(ref o)) if !o.success));
    assert_eq!(
        withdrawal.changes,
        [Change {
            path: "src/lib.rs".to_owned(),
            before: None,
            after: "pub fn f() {}\n".to_owned(),
        }]
    );
    assert_eq!(
        main_line.messages().len(),
        messages_before,
        "nothing goes to the model"
    );
    assert_eq!(client.requests().len(), 3);
}

#[test]
fn withdrawing_a_request_that_wrote_nothing_runs_no_check() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    let with_dbg = "fn main() {\n    dbg!(1);\n}\n";
    client.push_chat(Ok(write_file("d1", "src/main.rs", with_dbg)));
    client.push_chat(Ok(write_file("d2", "src/main.rs", with_dbg)));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());
    main_line.turn("debug 1", &mut Answer::no());

    let withdrawal = main_line.withdraw();

    assert!(cargo.calls().is_empty());
    assert!(withdrawal.changes.is_empty());
    assert!(withdrawal.check.is_none());
}

#[test]
fn a_held_turn_is_withdrawn_once() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    wrote_then_boundary(&client);
    cargo.push(passed());
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());
    main_line.turn("debug 1", &mut Answer::no());

    main_line.withdraw();
    let again = main_line.withdraw();

    assert_eq!(cargo.calls(), checks(1));
    assert!(again.changes.is_empty() && again.check.is_none());
}

#[test]
fn after_a_stop_molt_checks_but_asks_to_add_no_crate() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    client.push_chat(Ok(write_file("w", "src/main.rs", USES_SERDE_JSON)));
    client.push_chat(Err(crate::llm::ChatError::Status {
        status: 500,
        body: "down".to_owned(),
    }));
    cargo.push(failed(MISMATCH));
    let mut main_line = main_line(&client, &cargo, &dir, known_crate("serde_json"));
    let mut approver = Answer::yes();

    let turn = main_line.turn("serialize 1", &mut approver);

    assert!(matches!(turn.end, TurnEnd::ApiFailed(_)), "{:?}", turn.end);
    assert!(approver.asked.is_empty(), "{:?}", approver.asked);
    assert_eq!(cargo.calls(), checks(1));
    assert_eq!(
        fs::read_to_string(dir.path().join("Cargo.toml")).unwrap(),
        MANIFEST_TEXT
    );
}

#[test]
fn after_a_reply_without_tool_calls_molt_check_joins_that_reply() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    client.push_chat(Ok(write_file("w", "src/main.rs", CLEAN)));
    client.push_chat(Ok(reply("done")));
    client.push_chat(Ok(reply("gave up")));
    cargo.push(failed(MISMATCH));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

    main_line.turn("print 1", &mut Answer::no());

    let sent = &client.requests()[2].messages;
    for pair in sent.windows(2) {
        assert!(
            !matches!(pair, [Message::Assistant { .. }, Message::Assistant { .. }]),
            "{sent:?}"
        );
    }
    assert!(
        sent.iter().any(|m| matches!(
            m,
            Message::Assistant { content: Some(c), tool_calls }
                if c == "done" && tool_calls[0].id == "molt_check_1"
        )),
        "{sent:?}"
    );
}
