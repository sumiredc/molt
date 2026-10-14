//! Tests of ① and ② on the side line and what the main line does with them (PRD UC3, §7, §8.5).
//! Named so that `cargo test subline::resolve` selects them.

use std::fs;

use serde_json::json;
use tempfile::TempDir;

use super::*;
use crate::agent::mainline::{MainLine, TurnEnd};
use crate::agent::tools::{ApprovalRequest, Approver, Tools};
use crate::config::Config;
use crate::llm::{ChatRequest, ChatResponse, FakeChatClient, ToolCall};
use crate::profile::status::{Provenance, Status};
use crate::profile::store::Profile;
use crate::workspace::cargo::{CargoOutput, FakeCargo};
use crate::workspace::path::Root;

const WITH_DBG: &str = "fn main() {\n    dbg!(1);\n}\n";
const WITHOUT_DBG: &str = "fn main() {\n    let _ = 1;\n}\n";

struct Refuse;

impl Approver for Refuse {
    fn confirm(&mut self, _request: &ApprovalRequest) -> bool {
        false
    }
}

fn dbg_macro() -> ItemKey {
    "dbg_macro".parse().unwrap()
}

fn project() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"demo\"\n",
    )
    .unwrap();
    fs::create_dir(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
    dir
}

/// A cargo whose `cargo check` passes every time.
fn cargo() -> FakeCargo {
    let cargo = FakeCargo::new();
    for _ in 0..4 {
        cargo.push(Ok(CargoOutput {
            success: true,
            stdout: String::new(),
            stderr: String::new(),
        }));
    }
    cargo
}

fn main_line<'a>(client: &'a FakeChatClient, cargo: &'a FakeCargo, dir: &TempDir) -> MainLine<'a> {
    let tools = Tools::new(Root::new(dir.path()).unwrap(), cargo);
    MainLine::new(
        client,
        Router::new(&Config::default()),
        tools,
        Profile::new(),
    )
}

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

/// `Cargo.toml` of the project with `serde-json` among its dependencies.
const WITH_SERDE_JSON_DEPENDENCY: &str =
    "[package]\nname = \"demo\"\n\n[dependencies]\nserde-json = \"1\"\n";

fn write_manifest(id: &str, content: &str) -> ChatResponse {
    ChatResponse {
        content: None,
        tool_calls: vec![ToolCall {
            id: id.to_owned(),
            name: "write_file".to_owned(),
            arguments: json!({ "path": "Cargo.toml", "content": content }).to_string(),
        }],
    }
}

fn reply(text: &str) -> ChatResponse {
    ChatResponse {
        content: Some(text.to_owned()),
        tool_calls: Vec::new(),
    }
}

/// Runs a turn on "debug 1" that the model's two `dbg!` writes stop at a boundary.
fn stop_at_a_boundary(client: &FakeChatClient, main_line: &mut MainLine) -> Boundary {
    client.push_chat(Ok(write_main("first", WITH_DBG)));
    match main_line.turn("debug 1", &mut Refuse).end {
        TurnEnd::Boundary(boundary) => boundary,
        end => panic!("the turn did not stop at a boundary: {end:?}"),
    }
}

/// The user messages of `request`, in order.
fn user_messages(request: &ChatRequest) -> Vec<String> {
    request
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::User { content } => Some(content.clone()),
            _ => None,
        })
        .collect()
}

fn generate_requests(client: &FakeChatClient) -> Vec<ChatRequest> {
    let generate = Config::default().models.generate;
    client
        .requests()
        .into_iter()
        .filter(|request| request.model == generate)
        .collect()
}

#[test]
fn unlock_makes_the_items_known_with_the_side_line_as_the_provenance() {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = cargo();
    let mut main_line = main_line(&client, &cargo, &dir);
    let boundary = stop_at_a_boundary(&client, &mut main_line);
    let side = main_line.side_line(boundary);
    let resolution = side
        .resolve(Choice::Unlock, main_line.tools(), &SessionLog::disabled())
        .unwrap();
    client.push_chat(Ok(reply("done")));

    main_line.resolve(&resolution, &mut Refuse).unwrap();

    let entry = main_line.profile().entry(&dbg_macro()).unwrap();
    assert_eq!(entry.status(), Status::Known);
    assert_eq!(entry.provenance(), Provenance::SideLine);
}

#[test]
fn unlock_hands_the_main_line_one_message_with_unlock_and_the_item_names() {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = cargo();
    let mut main_line = main_line(&client, &cargo, &dir);
    let boundary = stop_at_a_boundary(&client, &mut main_line);
    let side = main_line.side_line(boundary);
    let resolution = side
        .resolve(Choice::Unlock, main_line.tools(), &SessionLog::disabled())
        .unwrap();
    client.push_chat(Ok(reply("done")));

    main_line.resolve(&resolution, &mut Refuse).unwrap();

    let requests = generate_requests(&client);
    let after = requests.last().unwrap();
    let before = &requests[requests.len() - 2];
    assert_eq!(
        after.messages.len(),
        before.messages.len() + 3,
        "the second write, its result, and one message from the side line"
    );
    let Some(Message::User { content }) = after.messages.last() else {
        panic!("not a user message last: {:?}", after.messages);
    };
    assert!(
        content.starts_with("BOUNDARY RESOLVED: unlock\n"),
        "{content}"
    );
    assert!(content.contains("`dbg_macro`"), "{content}");
}

#[test]
fn after_unlock_the_same_turn_goes_on_and_writes_the_items() {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = cargo();
    let mut main_line = main_line(&client, &cargo, &dir);
    let boundary = stop_at_a_boundary(&client, &mut main_line);
    let side = main_line.side_line(boundary);
    let resolution = side
        .resolve(Choice::Unlock, main_line.tools(), &SessionLog::disabled())
        .unwrap();
    client.push_chat(Ok(write_main("third", WITH_DBG)));
    client.push_chat(Ok(reply("done")));

    let resumed = main_line.resolve(&resolution, &mut Refuse).unwrap();

    assert!(matches!(resumed.turn.end, TurnEnd::Finished));
    assert_eq!(resumed.turn.tool_calls, ["write_file"; 2]);
    assert_eq!(
        fs::read_to_string(dir.path().join("src/main.rs")).unwrap(),
        WITH_DBG
    );
}

#[test]
fn the_profile_changed_by_a_choice_is_saved_to_the_profile_path() {
    let dir = project();
    let saved_to = dir.path().join("profile.toml");
    let client = FakeChatClient::new();
    let cargo = cargo();
    let mut main_line = main_line(&client, &cargo, &dir).with_profile_path(saved_to.clone());
    let boundary = stop_at_a_boundary(&client, &mut main_line);
    let side = main_line.side_line(boundary);
    let resolution = side
        .resolve(Choice::Unlock, main_line.tools(), &SessionLog::disabled())
        .unwrap();
    client.push_chat(Ok(reply("done")));

    let resumed = main_line.resolve(&resolution, &mut Refuse).unwrap();

    assert!(resumed.saved.is_ok());
    let saved = Profile::load(&saved_to).unwrap();
    assert_eq!(saved.status(&dbg_macro()), Status::Known);
}

#[test]
fn a_profile_that_cannot_be_saved_is_reported_and_the_choice_still_holds_in_the_session() {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = cargo();
    let mut main_line =
        main_line(&client, &cargo, &dir).with_profile_path(dir.path().join("no/such/profile.toml"));
    let boundary = stop_at_a_boundary(&client, &mut main_line);
    let side = main_line.side_line(boundary);
    let resolution = side
        .resolve(Choice::Unlock, main_line.tools(), &SessionLog::disabled())
        .unwrap();
    client.push_chat(Ok(reply("done")));

    let resumed = main_line.resolve(&resolution, &mut Refuse).unwrap();

    assert!(resumed.saved.is_err());
    assert_eq!(main_line.profile().status(&dbg_macro()), Status::Known);
}

#[test]
fn after_unlock_the_item_is_not_a_boundary_again_in_the_session() {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = cargo();
    let mut main_line = main_line(&client, &cargo, &dir);
    let boundary = stop_at_a_boundary(&client, &mut main_line);
    let side = main_line.side_line(boundary);
    let resolution = side
        .resolve(Choice::Unlock, main_line.tools(), &SessionLog::disabled())
        .unwrap();
    client.push_chat(Ok(reply("done")));
    main_line.resolve(&resolution, &mut Refuse).unwrap();
    client.push_chat(Ok(write_main("w1", WITH_DBG)));
    client.push_chat(Ok(write_main("w2", WITH_DBG)));
    client.push_chat(Ok(reply("done")));

    let turn = main_line.turn("debug 2", &mut Refuse);

    assert!(matches!(turn.end, TurnEnd::Finished), "{:?}", turn.end);
    assert_eq!(turn.written, ["src/main.rs"]);
}

#[test]
fn rewrite_makes_the_items_seen_with_the_side_line_as_the_provenance() {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = cargo();
    let mut main_line = main_line(&client, &cargo, &dir);
    let boundary = stop_at_a_boundary(&client, &mut main_line);
    let side = main_line.side_line(boundary);
    client.push_chat(Ok(reply("Print with println! instead.")));
    let resolution = side
        .resolve(Choice::Rewrite, main_line.tools(), &SessionLog::disabled())
        .unwrap();
    client.push_chat(Ok(reply("done")));

    main_line.resolve(&resolution, &mut Refuse).unwrap();

    let entry = main_line.profile().entry(&dbg_macro()).unwrap();
    assert_eq!(entry.status(), Status::Seen);
    assert_eq!(entry.provenance(), Provenance::SideLine);
}

#[test]
fn rewrite_hands_the_main_line_the_rewrite_message_with_the_item_names_and_the_policy() {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = cargo();
    let mut main_line = main_line(&client, &cargo, &dir);
    let boundary = stop_at_a_boundary(&client, &mut main_line);
    let side = main_line.side_line(boundary);
    client.push_chat(Ok(reply("Print with println! instead.")));
    let resolution = side
        .resolve(Choice::Rewrite, main_line.tools(), &SessionLog::disabled())
        .unwrap();
    client.push_chat(Ok(write_main("third", WITHOUT_DBG)));
    client.push_chat(Ok(reply("done")));

    let resumed = main_line.resolve(&resolution, &mut Refuse).unwrap();

    assert!(matches!(resumed.turn.end, TurnEnd::Finished));
    let requests = generate_requests(&client);
    let users = user_messages(&requests[2]);
    let content = users.last().unwrap();
    assert!(
        content.starts_with("BOUNDARY RESOLVED: rewrite\n"),
        "{content}"
    );
    assert!(content.contains("`dbg_macro`"), "{content}");
    assert!(
        content.contains("Policy: Print with println! instead.\n"),
        "{content}"
    );
}

#[test]
fn the_policy_of_rewrite_is_asked_of_the_light_model_with_the_side_line_transcript() {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = cargo();
    let mut main_line = main_line(&client, &cargo, &dir);
    let boundary = stop_at_a_boundary(&client, &mut main_line);
    let mut side = main_line.side_line(boundary);
    client.push_chat(Ok(reply("EXPLANATION")));
    client.push_chat(Ok(reply("ANSWER")));
    client.push_chat(Ok(reply("\n  Print with println! instead.  \nmore\n")));
    side.explain(&SessionLog::disabled()).unwrap();
    side.ask("QUESTION", &SessionLog::disabled()).unwrap();

    let resolution = side
        .resolve(Choice::Rewrite, main_line.tools(), &SessionLog::disabled())
        .unwrap();

    assert_eq!(resolution.policy(), Some("Print with println! instead."));
    let request = client.requests().pop().unwrap();
    assert_eq!(request.model, Config::default().models.light);
    let said = format!("{:?}", request.messages);
    for text in ["EXPLANATION", "QUESTION", "ANSWER"] {
        assert!(said.contains(text), "{text} not in {said}");
    }
}

#[test]
fn the_policy_of_rewrite_is_the_fixed_one_when_the_light_model_fails_or_says_nothing() {
    for answer in [
        Err(ChatError::FakeExhausted),
        Ok(reply(" \n")),
        Ok(ChatResponse {
            content: None,
            tool_calls: Vec::new(),
        }),
    ] {
        let dir = project();
        let client = FakeChatClient::new();
        let cargo = cargo();
        let mut main_line = main_line(&client, &cargo, &dir);
        let boundary = stop_at_a_boundary(&client, &mut main_line);
        let side = main_line.side_line(boundary);
        client.push_chat(answer);

        let resolution = side
            .resolve(Choice::Rewrite, main_line.tools(), &SessionLog::disabled())
            .unwrap();

        assert_eq!(
            resolution.policy(),
            Some(prompts::fixed_policy(&[dbg_macro()]).as_str())
        );
    }
}

#[test]
fn the_side_line_transcript_does_not_reach_the_main_line_after_a_choice() {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = cargo();
    let mut main_line = main_line(&client, &cargo, &dir);
    let boundary = stop_at_a_boundary(&client, &mut main_line);
    let mut side = main_line.side_line(boundary);
    client.push_chat(Ok(reply("EXPLANATION")));
    client.push_chat(Ok(reply("ANSWER")));
    client.push_chat(Ok(reply("POLICY")));
    side.explain(&SessionLog::disabled()).unwrap();
    side.ask("QUESTION", &SessionLog::disabled()).unwrap();
    let resolution = side
        .resolve(Choice::Rewrite, main_line.tools(), &SessionLog::disabled())
        .unwrap();
    client.push_chat(Ok(reply("done")));

    main_line.resolve(&resolution, &mut Refuse).unwrap();

    let said = format!("{:?}", generate_requests(&client).last().unwrap().messages);
    for text in ["EXPLANATION", "QUESTION", "ANSWER"] {
        assert!(!said.contains(text), "{text} in the main line: {said}");
    }
}

#[test]
fn a_boundary_that_recurs_after_rewrite_in_the_same_turn_does_not_offer_rewrite() {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = cargo();
    let mut main_line = main_line(&client, &cargo, &dir);
    let boundary = stop_at_a_boundary(&client, &mut main_line);
    let side = main_line.side_line(boundary);
    client.push_chat(Ok(reply("policy")));
    let resolution = side
        .resolve(Choice::Rewrite, main_line.tools(), &SessionLog::disabled())
        .unwrap();
    client.push_chat(Ok(write_main("third", WITH_DBG)));

    let resumed = main_line.resolve(&resolution, &mut Refuse).unwrap();

    let TurnEnd::Boundary(again) = resumed.turn.end else {
        panic!("the turn did not stop again: {:?}", resumed.turn.end);
    };
    assert_eq!(again.items, [dbg_macro()]);
    let side = main_line.side_line(again);
    assert!(side.is_recurrence());
    assert_eq!(
        side.choices(),
        [Choice::Unlock, Choice::UserWrote, Choice::Next]
    );
    assert_eq!(
        side.resolve(Choice::Rewrite, main_line.tools(), &SessionLog::disabled()),
        Err(Unresolved::NotOffered)
    );
}

#[test]
fn the_same_boundary_in_a_later_turn_after_rewrite_is_not_a_recurrence() {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = cargo();
    let mut main_line = main_line(&client, &cargo, &dir);
    let boundary = stop_at_a_boundary(&client, &mut main_line);
    let side = main_line.side_line(boundary);
    client.push_chat(Ok(reply("policy")));
    let resolution = side
        .resolve(Choice::Rewrite, main_line.tools(), &SessionLog::disabled())
        .unwrap();
    client.push_chat(Ok(reply("done")));
    main_line.resolve(&resolution, &mut Refuse).unwrap();

    let boundary = stop_at_a_boundary(&client, &mut main_line);

    let side = main_line.side_line(boundary);
    assert!(!side.is_recurrence());
    assert_eq!(
        side.choices(),
        [
            Choice::Unlock,
            Choice::Rewrite,
            Choice::UserWrote,
            Choice::Next
        ]
    );
}

#[test]
fn nothing_is_resolved_when_no_turn_is_held_at_a_boundary() {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = cargo();
    let mut main_line = main_line(&client, &cargo, &dir);
    let boundary = stop_at_a_boundary(&client, &mut main_line);
    let side = main_line.side_line(boundary);
    let resolution = side
        .resolve(Choice::Unlock, main_line.tools(), &SessionLog::disabled())
        .unwrap();
    main_line.withdraw();

    assert!(main_line.resolve(&resolution, &mut Refuse).is_none());
    assert_eq!(main_line.profile().status(&dbg_macro()), Status::Unknown);
}

#[test]
fn a_boundary_after_rewrite_that_names_a_rewritten_item_with_a_new_one_is_a_recurrence() {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = cargo();
    let mut main_line = main_line(&client, &cargo, &dir);
    let boundary = stop_at_a_boundary(&client, &mut main_line);
    let side = main_line.side_line(boundary);
    client.push_chat(Ok(reply("policy")));
    let resolution = side
        .resolve(Choice::Rewrite, main_line.tools(), &SessionLog::disabled())
        .unwrap();
    client.push_chat(Ok(write_main(
        "both",
        "fn main() {\n    dbg!(matches!(1, 1));\n}\n",
    )));

    let resumed = main_line.resolve(&resolution, &mut Refuse).unwrap();

    let TurnEnd::Boundary(again) = resumed.turn.end else {
        panic!("the turn did not stop again: {:?}", resumed.turn.end);
    };
    assert_eq!(again.items.len(), 2);
    assert_eq!(
        main_line.side_line(again).choices(),
        [Choice::Unlock, Choice::UserWrote, Choice::Next]
    );
}

#[test]
fn rewrite_leaves_an_item_already_seen_as_it_is_and_adds_no_history() {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = cargo();
    let mut profile = Profile::new();
    profile
        .transition(
            dbg_macro(),
            Status::Seen,
            Provenance::SelfReport,
            crate::profile::store::Timestamp::now(),
            None,
        )
        .unwrap();
    let tools = Tools::new(Root::new(dir.path()).unwrap(), &cargo);
    let mut main_line = MainLine::new(&client, Router::new(&Config::default()), tools, profile);
    let boundary = stop_at_a_boundary(&client, &mut main_line);
    let side = main_line.side_line(boundary);
    client.push_chat(Ok(reply("policy")));
    let resolution = side
        .resolve(Choice::Rewrite, main_line.tools(), &SessionLog::disabled())
        .unwrap();
    client.push_chat(Ok(reply("done")));

    main_line.resolve(&resolution, &mut Refuse).unwrap();

    let entry = main_line.profile().entry(&dbg_macro()).unwrap();
    assert_eq!(entry.status(), Status::Seen);
    assert_eq!(entry.provenance(), Provenance::SelfReport);
    assert_eq!(main_line.profile().history().len(), 1);
}

#[test]
fn unlock_of_a_crate_unlocks_its_other_spelling_too_so_the_boundary_does_not_come_again() {
    const WITH_SERDE_JSON: &str = "fn f(_: serde_json::Value) {}\n\nfn main() {}\n";
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = cargo();
    let mut profile = Profile::new();
    profile
        .transition(
            "crate:serde-json".parse().unwrap(),
            Status::Seen,
            Provenance::SelfReport,
            crate::profile::store::Timestamp::now(),
            None,
        )
        .unwrap();
    let tools = Tools::new(Root::new(dir.path()).unwrap(), &cargo);
    let mut main_line = MainLine::new(&client, Router::new(&Config::default()), tools, profile);
    client.push_chat(Ok(write_main("first", WITH_SERDE_JSON)));
    let TurnEnd::Boundary(boundary) = main_line.turn("use serde_json", &mut Refuse).end else {
        panic!("the turn did not stop at a boundary");
    };
    assert_eq!(boundary.items, ["crate:serde_json".parse().unwrap()]);
    let side = main_line.side_line(boundary);
    let resolution = side
        .resolve(Choice::Unlock, main_line.tools(), &SessionLog::disabled())
        .unwrap();
    client.push_chat(Ok(write_main("third", WITH_SERDE_JSON)));
    client.push_chat(Ok(reply("done")));

    let resumed = main_line.resolve(&resolution, &mut Refuse).unwrap();

    assert!(
        matches!(resumed.turn.end, TurnEnd::Finished),
        "{:?}",
        resumed.turn.end
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("src/main.rs")).unwrap(),
        WITH_SERDE_JSON
    );
}

#[test]
fn unlock_of_a_crate_unlocks_its_spelling_with_a_hyphen_in_the_profile_too() {
    const WITH_SERDE_JSON: &str = "fn f(_: serde_json::Value) {}\n\nfn main() {}\n";
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = cargo();
    let mut profile = Profile::new();
    profile
        .transition(
            "crate:serde-json".parse().unwrap(),
            Status::Seen,
            Provenance::SelfReport,
            crate::profile::store::Timestamp::now(),
            None,
        )
        .unwrap();
    let tools = Tools::new(Root::new(dir.path()).unwrap(), &cargo);
    let mut main_line = MainLine::new(&client, Router::new(&Config::default()), tools, profile);
    client.push_chat(Ok(write_manifest("asked", WITH_SERDE_JSON_DEPENDENCY)));
    let TurnEnd::Boundary(boundary) = main_line.turn("parse JSON", &mut Refuse).end else {
        panic!("the turn did not stop at a boundary");
    };
    assert_eq!(boundary.items, ["crate:serde_json".parse().unwrap()]);
    let side = main_line.side_line(boundary);
    let resolution = side
        .resolve(Choice::Unlock, main_line.tools(), &SessionLog::disabled())
        .unwrap();
    client.push_chat(Ok(write_main("w1", WITH_SERDE_JSON)));
    client.push_chat(Ok(reply("done")));

    let resumed = main_line.resolve(&resolution, &mut Refuse).unwrap();

    assert!(
        matches!(resumed.turn.end, TurnEnd::Finished),
        "{:?}",
        resumed.turn.end
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("src/main.rs")).unwrap(),
        WITH_SERDE_JSON
    );
}

#[test]
fn a_crate_in_cargo_toml_with_a_hyphen_after_rewrite_of_its_use_in_code_is_a_recurrence() {
    const WITH_SERDE_JSON: &str = "fn f(_: serde_json::Value) {}\n\nfn main() {}\n";
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = cargo();
    let mut main_line = main_line(&client, &cargo, &dir);
    client.push_chat(Ok(write_main("first", WITH_SERDE_JSON)));
    let TurnEnd::Boundary(boundary) = main_line.turn("use serde_json", &mut Refuse).end else {
        panic!("the turn did not stop at a boundary");
    };
    assert_eq!(boundary.items, ["crate:serde_json".parse().unwrap()]);
    let side = main_line.side_line(boundary);
    client.push_chat(Ok(reply("policy")));
    let resolution = side
        .resolve(Choice::Rewrite, main_line.tools(), &SessionLog::disabled())
        .unwrap();
    client.push_chat(Ok(write_manifest("asked", WITH_SERDE_JSON_DEPENDENCY)));

    let resumed = main_line.resolve(&resolution, &mut Refuse).unwrap();

    let TurnEnd::Boundary(again) = resumed.turn.end else {
        panic!("the turn did not stop again: {:?}", resumed.turn.end);
    };
    assert!(main_line.side_line(again).is_recurrence());
}

#[test]
fn unlock_makes_an_item_already_seen_known() {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = cargo();
    let mut profile = Profile::new();
    profile
        .transition(
            dbg_macro(),
            Status::Seen,
            Provenance::SelfReport,
            crate::profile::store::Timestamp::now(),
            None,
        )
        .unwrap();
    let tools = Tools::new(Root::new(dir.path()).unwrap(), &cargo);
    let mut main_line = MainLine::new(&client, Router::new(&Config::default()), tools, profile);
    let boundary = stop_at_a_boundary(&client, &mut main_line);
    let side = main_line.side_line(boundary);
    let resolution = side
        .resolve(Choice::Unlock, main_line.tools(), &SessionLog::disabled())
        .unwrap();
    client.push_chat(Ok(reply("done")));

    main_line.resolve(&resolution, &mut Refuse).unwrap();

    let entry = main_line.profile().entry(&dbg_macro()).unwrap();
    assert_eq!(entry.status(), Status::Known);
    assert_eq!(entry.provenance(), Provenance::SideLine);
}

#[test]
fn after_rewrite_the_file_is_back_and_the_end_of_the_turn_checks_though_nothing_more_was_written() {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = cargo();
    let mut main_line = main_line(&client, &cargo, &dir);
    let boundary = stop_at_a_boundary(&client, &mut main_line);
    let side = main_line.side_line(boundary);
    client.push_chat(Ok(reply("policy")));
    let resolution = side
        .resolve(Choice::Rewrite, main_line.tools(), &SessionLog::disabled())
        .unwrap();
    client.push_chat(Ok(reply("I will leave it as it was.")));

    let resumed = main_line.resolve(&resolution, &mut Refuse).unwrap();

    assert_eq!(
        fs::read_to_string(dir.path().join("src/main.rs")).unwrap(),
        "fn main() {}\n",
        "the step is taken back"
    );
    assert!(resumed.taken_back.is_some_and(|taken| taken.restored));
    assert!(
        resumed.turn.check.is_some(),
        "PRD §8.4: a step taken back counts"
    );
}

#[test]
fn next_leaves_a_seen_item_as_it_is_and_logs_no_change_of_it() {
    let dir = project();
    let logs = tempfile::tempdir().unwrap();
    let client = FakeChatClient::new();
    let cargo = cargo();
    let mut profile = Profile::new();
    profile
        .transition(
            dbg_macro(),
            Status::Seen,
            Provenance::SelfReport,
            crate::profile::store::Timestamp::now(),
            None,
        )
        .unwrap();
    let tools = Tools::new(Root::new(dir.path()).unwrap(), &cargo);
    let log = SessionLog::create(logs.path(), false).unwrap();
    let path = log.path().unwrap().to_owned();
    let mut main_line =
        MainLine::new(&client, Router::new(&Config::default()), tools, profile).with_log(log);
    let boundary = stop_at_a_boundary(&client, &mut main_line);
    let side = main_line.side_line(boundary);
    let resolution = side
        .resolve(Choice::Next, main_line.tools(), &SessionLog::disabled())
        .unwrap();
    client.push_chat(Ok(reply("done")));

    main_line.resolve(&resolution, &mut Refuse).unwrap();

    let entry = main_line.profile().entry(&dbg_macro()).unwrap();
    assert_eq!(
        (entry.status(), entry.provenance()),
        (Status::Seen, Provenance::SelfReport)
    );
    let log = fs::read_to_string(path).unwrap();
    assert!(log.contains(r#""choice":"next""#), "{log}");
    assert!(!log.contains("profile_change"), "{log}");
}

#[test]
fn a_crate_unlocked_may_be_added_and_one_chosen_rewrite_may_not() {
    for (choice, asked) in [(Choice::Unlock, true), (Choice::Rewrite, false)] {
        let dir = project();
        let client = FakeChatClient::new();
        let cargo = cargo();
        let mut main_line = main_line(&client, &cargo, &dir);
        client.push_chat(Ok(write_main(
            "step",
            "fn main() {\n    let _ = serde::VALUE;\n}\n",
        )));
        let TurnEnd::Boundary(boundary) = main_line.turn("use serde", &mut Refuse).end else {
            panic!("the turn did not stop at a boundary");
        };
        let side = main_line.side_line(boundary);
        if choice == Choice::Rewrite {
            client.push_chat(Ok(reply("policy")));
        }
        let resolution = side
            .resolve(choice, main_line.tools(), &SessionLog::disabled())
            .unwrap();
        client.push_chat(Ok(ChatResponse {
            content: None,
            tool_calls: vec![ToolCall {
                id: "add".to_owned(),
                name: "run_cargo".to_owned(),
                arguments: json!({ "subcommand": "add", "args": ["serde"] }).to_string(),
            }],
        }));
        client.push_chat(Ok(reply("done")));

        main_line.resolve(&resolution, &mut Refuse).unwrap();

        let result = generate_requests(&client)
            .last()
            .unwrap()
            .messages
            .iter()
            .find_map(|message| match message {
                Message::Tool {
                    tool_call_id,
                    content,
                } if tool_call_id == "add" => Some(content.clone()),
                _ => None,
            })
            .unwrap();
        // Asked and refused by the user, or refused before asking.
        assert_eq!(
            result == crate::agent::prompts::crate_add_before_step("serde"),
            !asked,
            "{choice:?}: {result}"
        );
    }
}

#[test]
fn a_step_that_could_not_be_taken_back_is_noted_in_the_message_of_rewrite() {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = cargo();
    let mut main_line = main_line(&client, &cargo, &dir);
    let boundary = stop_at_a_boundary(&client, &mut main_line);
    fs::write(dir.path().join("src/main.rs"), "// changed while paused\n").unwrap();
    let side = main_line.side_line(boundary);
    client.push_chat(Ok(reply("policy")));
    let resolution = side
        .resolve(Choice::Rewrite, main_line.tools(), &SessionLog::disabled())
        .unwrap();
    client.push_chat(Ok(reply("done")));

    let resumed = main_line.resolve(&resolution, &mut Refuse).unwrap();

    assert!(resumed.taken_back.is_some_and(|taken| !taken.restored));
    let message = user_messages(generate_requests(&client).last().unwrap())
        .pop()
        .unwrap();
    assert!(
        message.contains("could not be taken back") && message.contains("`src/main.rs`"),
        "{message}"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("src/main.rs")).unwrap(),
        "// changed while paused\n"
    );
}
