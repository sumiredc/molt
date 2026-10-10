//! Tests of ③ on the side line and what the main line does with it (PRD UC3, §7, §8.5). Named so
//! that `cargo test subline::user_wrote` selects them.

use std::fs;

use serde_json::json;
use tempfile::TempDir;

use super::super::*;
use super::{Done, Failure};
use crate::agent::mainline::{MainLine, TurnEnd};
use crate::agent::tools::{ApprovalRequest, Approver, Tools};
use crate::config::Config;
use crate::llm::{ChatRequest, ChatResponse, FakeChatClient, ToolCall};
use crate::profile::status::{Provenance, Status};
use crate::profile::store::{Profile, Timestamp};
use crate::workspace::cargo::{CargoCall, CargoError, CargoOutput, FakeCargo, Subcommand};
use crate::workspace::path::Root;

const WITH_DBG: &str = "fn main() {\n    dbg!(1);\n}\n";
const MANIFEST: &str = "[package]\nname = \"demo\"\n";

struct Refuse;

impl Approver for Refuse {
    fn confirm(&mut self, _request: &ApprovalRequest) -> bool {
        false
    }
}

fn key(name: &str) -> ItemKey {
    name.parse().unwrap()
}

fn project() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("Cargo.toml"), MANIFEST).unwrap();
    fs::create_dir(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
    dir
}

fn output(success: bool, stderr: &str) -> CargoOutput {
    CargoOutput {
        success,
        stdout: String::new(),
        stderr: stderr.to_owned(),
    }
}

/// A cargo whose `cargo check` gives each of `passes` in turn.
fn cargo(passes: &[bool]) -> FakeCargo {
    let cargo = FakeCargo::new();
    for &passed in passes {
        cargo.push(Ok(output(passed, if passed { "" } else { "error[E0425]" })));
    }
    cargo
}

fn main_line<'a>(
    client: &'a FakeChatClient,
    cargo: &'a FakeCargo,
    dir: &TempDir,
    profile: Profile,
) -> MainLine<'a> {
    let tools = Tools::new(Root::new(dir.path()).unwrap(), cargo);
    MainLine::new(client, Router::new(&Config::default()), tools, profile)
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

fn reply(text: &str) -> ChatResponse {
    ChatResponse {
        content: Some(text.to_owned()),
        tool_calls: Vec::new(),
    }
}

/// Runs a turn on "debug 1" that the model's two `dbg!` writes stop at a boundary.
fn stop_at_a_boundary(client: &FakeChatClient, main_line: &mut MainLine) -> Boundary {
    client.push_chat(Ok(write_main("first", WITH_DBG)));
    client.push_chat(Ok(write_main("second", WITH_DBG)));
    stop(client, main_line)
}

/// Runs a turn on "debug 1" that the model stops at a boundary on `items` with `request_boundary`.
fn request_boundary(client: &FakeChatClient, main_line: &mut MainLine, items: &[&str]) -> Boundary {
    client.push_chat(Ok(ChatResponse {
        content: None,
        tool_calls: vec![ToolCall {
            id: "boundary".to_owned(),
            name: "request_boundary".to_owned(),
            arguments: json!({ "items": items, "reason": "needed" }).to_string(),
        }],
    }));
    stop(client, main_line)
}

fn stop(_client: &FakeChatClient, main_line: &mut MainLine) -> Boundary {
    match main_line.turn("debug 1", &mut Refuse).end {
        TurnEnd::Boundary(boundary) => boundary,
        end => panic!("the turn did not stop at a boundary: {end:?}"),
    }
}

/// The resolution of a `/done` that passed.
fn passed(done: Done) -> Resolution {
    match done {
        Done::Passed { resolution, .. } => resolution,
        done => panic!("/done did not pass: {done:?}"),
    }
}

fn failed(done: Done) -> Failure {
    match done {
        Done::Failed(failure) => failure,
        done => panic!("/done did not fail: {done:?}"),
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
fn user_wrote_makes_the_items_in_the_users_lines_known_with_wrote_it_as_the_provenance() {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = cargo(&[true]);
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());
    let boundary = stop_at_a_boundary(&client, &mut main_line);
    let mut side = main_line.side_line(boundary);
    fs::write(dir.path().join("src/main.rs"), WITH_DBG).unwrap();
    client.push_chat(Ok(reply("review")));
    let resolution = passed(main_line.done(&mut side));
    client.push_chat(Ok(reply("done")));

    main_line.resolve(&resolution, &mut Refuse).unwrap();

    assert_eq!(resolution.choice(), Choice::UserWrote);
    assert_eq!(resolution.items(), [key("dbg_macro")]);
    let entry = main_line.profile().entry(&key("dbg_macro")).unwrap();
    assert_eq!(entry.status(), Status::Known);
    assert_eq!(entry.provenance(), Provenance::WroteIt);
}

#[test]
fn user_wrote_hands_the_main_line_one_message_with_user_wrote_the_item_names_and_the_files_to_re_read()
 {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = cargo(&[true]);
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());
    let boundary = stop_at_a_boundary(&client, &mut main_line);
    let mut side = main_line.side_line(boundary);
    client.push_chat(Ok(reply("GUIDANCE: write it in main")));
    side.guide(&SessionLog::disabled()).unwrap();
    fs::write(dir.path().join("src/main.rs"), WITH_DBG).unwrap();
    client.push_chat(Ok(reply("REVIEW: fine")));
    let resolution = passed(main_line.done(&mut side));
    client.push_chat(Ok(reply("done")));
    let before = user_messages(generate_requests(&client).last().unwrap());

    main_line.resolve(&resolution, &mut Refuse).unwrap();

    let after = user_messages(generate_requests(&client).last().unwrap());
    assert_eq!(after.len(), before.len() + 1, "{after:?}");
    let message = after.last().unwrap();
    assert_eq!(message, &resolution.message());
    assert!(
        message.starts_with("BOUNDARY RESOLVED: user_wrote\n"),
        "{message}"
    );
    assert!(message.contains("`dbg_macro`"), "{message}");
    assert!(message.contains("Re-read"), "{message}");
    assert!(message.contains("`src/main.rs`"), "{message}");
    for request in generate_requests(&client) {
        let text = format!("{:?}", request.messages);
        assert!(!text.contains("GUIDANCE"), "{text}");
        assert!(
            request.messages.len() == 2 || !text.contains("REVIEW"),
            "{text}"
        );
    }
}

#[test]
fn at_a_crate_boundary_the_crate_the_user_added_to_cargo_toml_becomes_known() {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = cargo(&[true]);
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());
    let boundary = request_boundary(&client, &mut main_line, &["crate:serde"]);
    let mut side = main_line.side_line(boundary);
    fs::write(
        dir.path().join("Cargo.toml"),
        format!("{MANIFEST}\n[dependencies]\nserde = \"1\"\n"),
    )
    .unwrap();
    client.push_chat(Ok(reply("review")));
    let resolution = passed(main_line.done(&mut side));
    client.push_chat(Ok(reply("done")));

    main_line.resolve(&resolution, &mut Refuse).unwrap();

    let entry = main_line.profile().entry(&key("crate:serde")).unwrap();
    assert_eq!(entry.status(), Status::Known);
    assert_eq!(entry.provenance(), Provenance::WroteIt);
}

#[test]
fn a_crate_the_user_wrote_unlocks_its_other_spelling_in_the_profile_too() {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = cargo(&[true]);
    let mut profile = Profile::new();
    profile
        .transition(
            key("crate:serde-json"),
            Status::Seen,
            Provenance::Manual,
            Timestamp::now(),
            None,
        )
        .unwrap();
    let mut main_line = main_line(&client, &cargo, &dir, profile);
    let boundary = request_boundary(&client, &mut main_line, &["crate:serde_json"]);
    let mut side = main_line.side_line(boundary);
    fs::write(
        dir.path().join("Cargo.toml"),
        format!("{MANIFEST}\n[dependencies]\nserde_json = \"1\"\n"),
    )
    .unwrap();
    client.push_chat(Ok(reply("review")));
    let resolution = passed(main_line.done(&mut side));
    client.push_chat(Ok(reply("done")));

    main_line.resolve(&resolution, &mut Refuse).unwrap();

    for spelling in ["crate:serde_json", "crate:serde-json"] {
        assert_eq!(
            main_line.profile().status(&key(spelling)),
            Status::Known,
            "{spelling}"
        );
    }
}

#[test]
fn an_item_on_a_line_the_user_did_not_change_does_not_become_known() {
    let dir = project();
    let lib = "use std::sync::Arc;\n\npub fn shared() -> Arc<u8> {\n    Arc::new(1)\n}\n";
    fs::write(dir.path().join("src/lib.rs"), lib).unwrap();
    let client = FakeChatClient::new();
    let cargo = cargo(&[true]);
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());
    let boundary = stop_at_a_boundary(&client, &mut main_line);
    let mut side = main_line.side_line(boundary);
    fs::write(
        dir.path().join("src/lib.rs"),
        format!("{lib}\npub fn debug() {{\n    dbg!(1);\n}}\n"),
    )
    .unwrap();
    client.push_chat(Ok(reply("review")));
    let resolution = passed(main_line.done(&mut side));
    client.push_chat(Ok(reply("done")));

    main_line.resolve(&resolution, &mut Refuse).unwrap();

    assert_eq!(resolution.items(), [key("dbg_macro")]);
    assert_eq!(main_line.profile().status(&key("arc")), Status::Unknown);
}

#[test]
fn a_failed_cargo_check_leaves_the_profile_unchanged_and_done_can_be_sent_again() {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = cargo(&[false, true]);
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());
    let boundary = stop_at_a_boundary(&client, &mut main_line);
    let mut side = main_line.side_line(boundary);
    side.writing = true;
    fs::write(
        dir.path().join("src/main.rs"),
        "fn main() {\n    dbg!(x);\n}\n",
    )
    .unwrap();

    let failure = failed(main_line.done(&mut side));

    assert!(
        matches!(&failure.check, Ok(output) if output.stderr == "error[E0425]"),
        "{failure:?}"
    );
    assert!(side.is_writing());
    assert_eq!(
        main_line.profile().status(&key("dbg_macro")),
        Status::Unknown
    );
    let saved = fs::read_to_string(dir.path().join("src/main.rs")).unwrap();
    assert!(saved.contains("dbg!(x)"), "the user's lines stay: {saved}");

    fs::write(dir.path().join("src/main.rs"), WITH_DBG).unwrap();
    client.push_chat(Ok(reply("review")));
    let resolution = passed(main_line.done(&mut side));
    client.push_chat(Ok(reply("done")));
    main_line.resolve(&resolution, &mut Refuse).unwrap();

    assert_eq!(main_line.profile().status(&key("dbg_macro")), Status::Known);
}

#[test]
fn after_a_failed_cargo_check_the_user_can_switch_to_unlock_or_rewrite() {
    for choice in [Choice::Unlock, Choice::Rewrite] {
        let dir = project();
        let client = FakeChatClient::new();
        let cargo = cargo(&[false]);
        let mut main_line = main_line(&client, &cargo, &dir, Profile::new());
        let boundary = stop_at_a_boundary(&client, &mut main_line);
        let mut side = main_line.side_line(boundary);
        fs::write(dir.path().join("src/main.rs"), WITH_DBG).unwrap();
        failed(main_line.done(&mut side));
        client.push_chat(Ok(reply("policy")));

        let resolution = side.resolve(choice, &SessionLog::disabled()).unwrap();
        client.push_chat(Ok(reply("done")));
        main_line.resolve(&resolution, &mut Refuse).unwrap();

        let entry = main_line.profile().entry(&key("dbg_macro")).unwrap();
        assert_eq!(entry.provenance(), Provenance::SideLine, "{choice:?}");
    }
}

#[test]
fn lines_that_do_not_parse_fail_done_and_leave_the_profile_unchanged() {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = cargo(&[false]);
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());
    let boundary = stop_at_a_boundary(&client, &mut main_line);
    let mut side = main_line.side_line(boundary);
    fs::write(dir.path().join("src/main.rs"), "fn main() {\n    dbg!(1)\n").unwrap();

    let failure = failed(main_line.done(&mut side));

    let [(path, _)] = &failure.parse_errors[..] else {
        panic!("not one parse error: {failure:?}");
    };
    assert_eq!(path, "src/main.rs");
    assert_eq!(
        main_line.profile().status(&key("dbg_macro")),
        Status::Unknown
    );
}

#[test]
fn a_cargo_check_that_cannot_run_fails_done() {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = FakeCargo::new();
    cargo.push(Err(CargoError::FakeExhausted));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());
    let boundary = stop_at_a_boundary(&client, &mut main_line);
    let mut side = main_line.side_line(boundary);
    fs::write(dir.path().join("src/main.rs"), WITH_DBG).unwrap();

    let failure = failed(main_line.done(&mut side));

    assert!(failure.check.is_err(), "{failure:?}");
    assert!(failure.parse_errors.is_empty());
}

#[test]
fn done_without_writing_anything_reports_no_change_and_the_user_chooses_again() {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = FakeCargo::new();
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());
    let boundary = stop_at_a_boundary(&client, &mut main_line);
    let mut side = main_line.side_line(boundary);
    client.push_chat(Ok(reply("guidance")));
    side.guide(&SessionLog::disabled()).unwrap();
    fs::write(dir.path().join("README.md"), "# not checkable\n").unwrap();

    assert!(matches!(main_line.done(&mut side), Done::NoChange));

    assert!(!side.is_writing());
    assert!(cargo.calls().is_empty(), "no cargo check");
    assert_eq!(
        side.choices(),
        [Choice::Unlock, Choice::Rewrite, Choice::UserWrote]
    );
}

#[test]
fn the_review_is_asked_of_the_generate_model_with_the_users_lines_and_the_request() {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = cargo(&[true]);
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());
    let boundary = stop_at_a_boundary(&client, &mut main_line);
    let mut side = main_line.side_line(boundary);
    fs::write(dir.path().join("src/main.rs"), WITH_DBG).unwrap();
    client.push_chat(Ok(reply("Looks fine.")));

    let Done::Passed { review, .. } = main_line.done(&mut side) else {
        panic!("/done did not pass");
    };

    assert_eq!(review.unwrap(), "Looks fine.");
    assert_eq!(cargo.calls(), [CargoCall::Run(Subcommand::Check)]);
    let request = client.requests().pop().unwrap();
    assert_eq!(request.model, Config::default().models.generate);
    assert!(request.tools.is_empty());
    let [
        Message::System { content: system },
        Message::User { content },
    ] = &request.messages[..]
    else {
        panic!(
            "not a system prompt and one user message: {:?}",
            request.messages
        );
    };
    assert_eq!(system, prompts::REVIEW_SYSTEM);
    assert!(content.contains("+    dbg!(1);"), "{content}");
    assert!(content.contains("-fn main() {}"), "{content}");
    assert!(content.contains("debug 1"), "the user's request: {content}");
}

#[test]
fn a_failed_review_still_passes_done() {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = cargo(&[true]);
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());
    let boundary = stop_at_a_boundary(&client, &mut main_line);
    let mut side = main_line.side_line(boundary);
    fs::write(dir.path().join("src/main.rs"), WITH_DBG).unwrap();

    let Done::Passed { resolution, review } = main_line.done(&mut side) else {
        panic!("/done did not pass");
    };

    assert!(review.is_err());
    assert_eq!(resolution.items(), [key("dbg_macro")]);
}

#[test]
fn the_guidance_is_asked_of_the_reason_model_and_the_side_line_is_writing_from_then_on() {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = FakeCargo::new();
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());
    let boundary = stop_at_a_boundary(&client, &mut main_line);
    let mut side = main_line.side_line(boundary);
    client.push_chat(Ok(reply("explanation")));
    side.explain(&SessionLog::disabled()).unwrap();
    assert!(!side.is_writing());
    client.push_chat(Ok(reply("Write it in src/main.rs.")));

    assert_eq!(
        side.guide(&SessionLog::disabled()).unwrap(),
        "Write it in src/main.rs."
    );

    assert!(side.is_writing());
    let request = client.requests().pop().unwrap();
    assert_eq!(request.model, Config::default().models.reason);
    let last = user_messages(&request).pop().unwrap();
    assert_eq!(last, prompts::guidance_request(&[key("dbg_macro")]));
}

#[test]
fn the_files_the_user_wrote_join_the_changes_of_the_turn_with_their_content_from_the_boundary() {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = cargo(&[true]);
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());
    let boundary = stop_at_a_boundary(&client, &mut main_line);
    let mut side = main_line.side_line(boundary);
    fs::write(dir.path().join("src/main.rs"), WITH_DBG).unwrap();
    fs::write(dir.path().join("src/debug.rs"), "pub fn f() {}\n").unwrap();
    client.push_chat(Ok(reply("review")));
    let resolution = passed(main_line.done(&mut side));
    client.push_chat(Ok(reply("done")));

    let resumed = main_line.resolve(&resolution, &mut Refuse).unwrap();

    assert_eq!(resumed.turn.user_written, ["src/debug.rs", "src/main.rs"]);
    let changes: Vec<(&str, Option<&str>, &str)> = resumed
        .turn
        .changes
        .iter()
        .map(|c| (c.path.as_str(), c.before.as_deref(), c.after.as_str()))
        .collect();
    assert_eq!(
        changes,
        [
            ("src/debug.rs", None, "pub fn f() {}\n"),
            ("src/main.rs", Some("fn main() {}\n"), WITH_DBG),
        ]
    );
}

#[test]
fn choosing_user_wrote_is_not_resolved_without_done() {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = FakeCargo::new();
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());
    let boundary = stop_at_a_boundary(&client, &mut main_line);
    let side = main_line.side_line(boundary);

    assert_eq!(
        side.resolve(Choice::UserWrote, &SessionLog::disabled()),
        Err(Unresolved::NotOffered)
    );
}

#[test]
fn ctrl_c_while_waiting_for_the_review_changes_nothing_and_the_side_line_is_still_writing() {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = cargo(&[true, true]);
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());
    let boundary = stop_at_a_boundary(&client, &mut main_line);
    let mut side = main_line.side_line(boundary);
    client.push_chat(Ok(reply("guidance")));
    side.guide(&SessionLog::disabled()).unwrap();
    fs::write(dir.path().join("src/main.rs"), WITH_DBG).unwrap();
    client.push_chat(Err(ChatError::Interrupted));

    assert!(matches!(main_line.done(&mut side), Done::Interrupted));

    assert!(side.is_writing());
    assert_eq!(
        main_line.profile().status(&key("dbg_macro")),
        Status::Unknown
    );
    client.push_chat(Ok(reply("review")));
    assert!(matches!(main_line.done(&mut side), Done::Passed { .. }));
}

#[test]
fn ctrl_c_while_waiting_for_the_guidance_does_not_start_writing() {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = FakeCargo::new();
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());
    let boundary = stop_at_a_boundary(&client, &mut main_line);
    let mut side = main_line.side_line(boundary);
    client.push_chat(Err(ChatError::Interrupted));

    assert!(matches!(
        side.guide(&SessionLog::disabled()),
        Err(ChatError::Interrupted)
    ));

    assert!(!side.is_writing());
}

#[test]
fn the_cargo_lock_that_changed_with_the_users_crate_joins_their_files_but_not_the_review() {
    let dir = project();
    fs::write(dir.path().join("Cargo.lock"), "LOCK BEFORE\n").unwrap();
    let client = FakeChatClient::new();
    let cargo = cargo(&[true]);
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());
    let boundary = request_boundary(&client, &mut main_line, &["crate:serde"]);
    let mut side = main_line.side_line(boundary);
    fs::write(
        dir.path().join("Cargo.toml"),
        format!("{MANIFEST}\n[dependencies]\nserde = \"1\"\n"),
    )
    .unwrap();
    // What `cargo check` does to the lock after the user added a crate.
    fs::write(dir.path().join("Cargo.lock"), "LOCK AFTER\n").unwrap();
    client.push_chat(Ok(reply("review")));

    let resolution = passed(main_line.done(&mut side));

    let files: Vec<(&str, Option<&str>)> = resolution
        .files()
        .iter()
        .map(|file| (file.path.as_str(), file.before.as_deref()))
        .collect();
    assert_eq!(
        files,
        [
            ("Cargo.toml", Some(MANIFEST)),
            ("Cargo.lock", Some("LOCK BEFORE\n"))
        ]
    );
    assert_eq!(resolution.files()[1].after, "LOCK AFTER\n");
    let review = format!("{:?}", client.requests().pop().unwrap().messages);
    assert!(!review.contains("LOCK"), "{review}");
    let message = resolution.message();
    assert!(message.contains("`Cargo.toml`"), "{message}");
    assert!(!message.contains("Cargo.lock"), "not to re-read: {message}");
}

#[test]
fn lines_with_no_item_new_to_the_profile_pass_and_make_nothing_known() {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = cargo(&[true]);
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());
    let boundary = stop_at_a_boundary(&client, &mut main_line);
    let mut side = main_line.side_line(boundary);
    fs::write(
        dir.path().join("src/main.rs"),
        "fn main() {\n    let _x = 1;\n}\n",
    )
    .unwrap();
    client.push_chat(Ok(reply("review")));
    let resolution = passed(main_line.done(&mut side));
    client.push_chat(Ok(reply("done")));

    main_line.resolve(&resolution, &mut Refuse).unwrap();

    assert!(resolution.items().is_empty());
    assert!(
        resolution
            .message()
            .contains("Their lines use nothing new to the profile."),
        "{}",
        resolution.message()
    );
    assert_eq!(
        main_line.profile().status(&key("dbg_macro")),
        Status::Unknown
    );
}

#[test]
fn a_file_molt_wrote_before_the_boundary_and_the_user_changed_keeps_its_content_from_before_molt() {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = cargo(&[true, true]);
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());
    client.push_chat(Ok(ChatResponse {
        content: None,
        tool_calls: vec![ToolCall {
            id: "lib".to_owned(),
            name: "write_file".to_owned(),
            arguments: json!({ "path": "src/lib.rs", "content": "pub fn f() {}\n" }).to_string(),
        }],
    }));
    let boundary = stop_at_a_boundary(&client, &mut main_line);
    let mut side = main_line.side_line(boundary);
    let users = "pub fn f() {}\n\npub fn g() {\n    dbg!(1);\n}\n";
    fs::write(dir.path().join("src/lib.rs"), users).unwrap();
    client.push_chat(Ok(reply("review")));
    let resolution = passed(main_line.done(&mut side));
    client.push_chat(Ok(reply("done")));

    let resumed = main_line.resolve(&resolution, &mut Refuse).unwrap();

    assert_eq!(resumed.turn.user_written, ["src/lib.rs"]);
    let lib: Vec<(Option<&str>, &str)> = resumed
        .turn
        .changes
        .iter()
        .filter(|c| c.path == "src/lib.rs")
        .map(|c| (c.before.as_deref(), c.after.as_str()))
        .collect();
    assert_eq!(lib, [(None, users)], "created by molt in the turn");
}

#[test]
fn a_file_the_user_wrote_at_two_boundaries_of_a_turn_is_listed_once() {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = cargo(&[true, true]);
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());
    let boundary = stop_at_a_boundary(&client, &mut main_line);
    let mut side = main_line.side_line(boundary);
    fs::write(dir.path().join("src/main.rs"), WITH_DBG).unwrap();
    client.push_chat(Ok(reply("review")));
    let resolution = passed(main_line.done(&mut side));
    client.push_chat(Ok(ChatResponse {
        content: None,
        tool_calls: vec![ToolCall {
            id: "again".to_owned(),
            name: "request_boundary".to_owned(),
            arguments: json!({ "items": ["arc"], "reason": "shared" }).to_string(),
        }],
    }));
    let resumed = main_line.resolve(&resolution, &mut Refuse).unwrap();
    let TurnEnd::Boundary(again) = resumed.turn.end else {
        panic!("the turn did not stop again: {:?}", resumed.turn.end);
    };
    let mut side = main_line.side_line(again);
    let shared = "use std::sync::Arc;\n\nfn main() {\n    dbg!(Arc::new(1));\n}\n";
    fs::write(dir.path().join("src/main.rs"), shared).unwrap();
    client.push_chat(Ok(reply("review")));
    let resolution = passed(main_line.done(&mut side));
    client.push_chat(Ok(reply("done")));

    let resumed = main_line.resolve(&resolution, &mut Refuse).unwrap();

    assert_eq!(resumed.turn.user_written, ["src/main.rs"]);
    let main: Vec<(Option<&str>, &str)> = resumed
        .turn
        .changes
        .iter()
        .map(|c| (c.before.as_deref(), c.after.as_str()))
        .collect();
    assert_eq!(main, [(Some("fn main() {}\n"), shared)]);
    assert_eq!(main_line.profile().status(&key("arc")), Status::Known);
}
