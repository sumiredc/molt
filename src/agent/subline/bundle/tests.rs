//! Tests of several items at one boundary (PRD UC3's exception flow) and of the side-line events in
//! the session log (PRD §8.8). Named so that `cargo test subline::bundle` selects them.

use std::fs;
use std::path::Path;

use serde_json::{Value, json};
use tempfile::TempDir;

use super::super::{Choice, Done, Resolution};
use crate::agent::mainline::{Boundary, MainLine, Resolved, TurnEnd};
use crate::agent::tools::{ApprovalRequest, Approver, Tools};
use crate::config::Config;
use crate::llm::{ChatRequest, ChatResponse, FakeChatClient, Message, Router, ToolCall};
use crate::profile::status::{ItemKey, Provenance, Status};
use crate::profile::store::Profile;
use crate::session::SessionLog;
use crate::workspace::cargo::{CargoOutput, FakeCargo};
use crate::workspace::path::Root;

const WITH_DBG: &str = "fn main() {\n    dbg!(1);\n}\n";

struct Refuse;

impl Approver for Refuse {
    fn confirm(&mut self, _request: &ApprovalRequest) -> bool {
        false
    }
}

fn key(name: &str) -> ItemKey {
    name.parse().unwrap()
}

fn keys(names: &[&str]) -> Vec<ItemKey> {
    names.iter().map(|name| key(name)).collect()
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

fn reply(text: &str) -> ChatResponse {
    ChatResponse {
        content: Some(text.to_owned()),
        tool_calls: Vec::new(),
    }
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

/// Runs a turn on "share it" that the model stops at a boundary on `items` with
/// `request_boundary`.
fn stop_at(client: &FakeChatClient, main_line: &mut MainLine, items: &[&str]) -> Boundary {
    client.push_chat(Ok(ChatResponse {
        content: None,
        tool_calls: vec![ToolCall {
            id: "boundary".to_owned(),
            name: "request_boundary".to_owned(),
            arguments: json!({ "items": items, "reason": "needed" }).to_string(),
        }],
    }));
    match main_line.turn("share it", &mut Refuse).end {
        TurnEnd::Boundary(boundary) => boundary,
        end => panic!("the turn did not stop at a boundary: {end:?}"),
    }
}

/// The choice `choice` on the side line `main_line` opens at `boundary`, as `/choose` makes it.
fn choose(main_line: &MainLine, boundary: Boundary, choice: Choice) -> Resolution {
    main_line
        .side_line(boundary)
        .resolve(choice, &SessionLog::disabled())
        .unwrap()
}

fn pending(resolved: Resolved) -> Boundary {
    match resolved {
        Resolved::Pending(boundary) => boundary,
        Resolved::Resumed(resumed) => panic!("the turn went on: {resumed:?}"),
    }
}

fn generate_requests(client: &FakeChatClient) -> Vec<ChatRequest> {
    let generate = Config::default().models.generate;
    client
        .requests()
        .into_iter()
        .filter(|request| request.model == generate)
        .collect()
}

fn lines(path: &Path) -> Vec<Value> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn the_side_line_opens_for_the_items_of_the_category_of_the_first_item() {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = cargo();
    let mut main_line = main_line(&client, &cargo, &dir);
    let boundary = stop_at(&client, &mut main_line, &["arc", "dbg_macro", "mutex"]);

    let side = main_line.side_line(boundary);

    assert_eq!(side.boundary().items, keys(&["arc", "mutex"]));
}

#[test]
fn crates_are_bundled_as_one_category() {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = cargo();
    let mut main_line = main_line(&client, &cargo, &dir);
    let boundary = stop_at(
        &client,
        &mut main_line,
        &["crate:rand", "arc", "crate:serde"],
    );

    let side = main_line.side_line(boundary);

    assert_eq!(side.boundary().items, keys(&["crate:rand", "crate:serde"]));
}

#[test]
fn after_the_choice_for_one_category_the_items_of_the_others_come_back_and_nothing_changes() {
    let dir = project();
    let saved_to = dir.path().join("profile.toml");
    let client = FakeChatClient::new();
    let cargo = cargo();
    let mut main_line = main_line(&client, &cargo, &dir).with_profile_path(saved_to.clone());
    let boundary = stop_at(
        &client,
        &mut main_line,
        &["arc", "dbg_macro", "mutex", "matches_macro"],
    );
    let sent = client.requests().len();
    let resolution = choose(&main_line, boundary, Choice::Unlock);

    let next = pending(main_line.resolve(&resolution, &mut Refuse).unwrap());

    assert_eq!(next.items, keys(&["dbg_macro", "matches_macro"]));
    assert_eq!(client.requests().len(), sent, "the main line did not go on");
    assert_eq!(main_line.profile().status(&key("arc")), Status::Unknown);
    assert!(!saved_to.exists(), "the profile was not saved");
    assert_eq!(
        main_line.side_line(next).boundary().items,
        keys(&["dbg_macro", "matches_macro"])
    );
}

#[test]
fn once_every_category_has_a_choice_each_choice_holds_for_all_items_of_its_bundle() {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = cargo();
    let mut main_line = main_line(&client, &cargo, &dir);
    let boundary = stop_at(&client, &mut main_line, &["arc", "dbg_macro", "mutex"]);
    let unlock = choose(&main_line, boundary, Choice::Unlock);
    let next = pending(main_line.resolve(&unlock, &mut Refuse).unwrap());
    client.push_chat(Ok(reply("policy")));
    let rewrite = choose(&main_line, next, Choice::Rewrite);
    client.push_chat(Ok(reply("done")));

    let resumed = main_line.resolve(&rewrite, &mut Refuse).unwrap().resumed();

    assert!(matches!(resumed.turn.end, TurnEnd::Finished));
    assert_eq!(resumed.chosen, [unlock, rewrite]);
    let profile = main_line.profile();
    for (item, status) in [
        ("arc", Status::Known),
        ("mutex", Status::Known),
        ("dbg_macro", Status::Seen),
    ] {
        let entry = profile.entry(&key(item)).unwrap();
        assert_eq!((item, entry.status()), (item, status));
        assert_eq!(entry.provenance(), Provenance::SideLine);
    }
}

#[test]
fn the_main_line_gets_one_message_naming_the_items_of_each_bundle_in_the_order_chosen() {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = cargo();
    let mut main_line = main_line(&client, &cargo, &dir);
    let boundary = stop_at(&client, &mut main_line, &["arc", "dbg_macro", "mutex"]);
    let unlock = choose(&main_line, boundary, Choice::Unlock);
    let next = pending(main_line.resolve(&unlock, &mut Refuse).unwrap());
    client.push_chat(Ok(reply("Print with println! instead.")));
    let rewrite = choose(&main_line, next, Choice::Rewrite);
    client.push_chat(Ok(reply("done")));

    main_line.resolve(&rewrite, &mut Refuse).unwrap().resumed();

    let requests = generate_requests(&client);
    let (before, after) = (&requests[0], &requests[1]);
    assert_eq!(
        after.messages.len(),
        before.messages.len() + 3,
        "the boundary call, its result, and one message from the side lines"
    );
    let Some(Message::User { content }) = after.messages.last() else {
        panic!("not a user message last: {:?}", after.messages);
    };
    let unlock = content.find("BOUNDARY RESOLVED: unlock\n").expect(content);
    let rewrite = content.find("BOUNDARY RESOLVED: rewrite\n").expect(content);
    assert!(unlock < rewrite, "{content}");
    assert!(
        content[unlock..rewrite].contains("`arc`, `mutex`"),
        "{content}"
    );
    assert!(content[rewrite..].contains("`dbg_macro`"), "{content}");
    assert!(
        content[rewrite..].contains("Print with println! instead."),
        "{content}"
    );
}

#[test]
fn withdrawing_after_the_choice_for_one_category_changes_no_item() {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = cargo();
    let mut main_line = main_line(&client, &cargo, &dir);
    let boundary = stop_at(&client, &mut main_line, &["arc", "dbg_macro"]);
    let unlock = choose(&main_line, boundary, Choice::Unlock);
    pending(main_line.resolve(&unlock, &mut Refuse).unwrap());

    main_line.withdraw();

    assert_eq!(main_line.profile().status(&key("arc")), Status::Unknown);
    assert_eq!(
        main_line.profile().status(&key("dbg_macro")),
        Status::Unknown
    );
}

#[test]
fn withdrawing_after_write_it_myself_for_one_category_leaves_the_users_file_and_shows_no_change_of_it()
 {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = cargo();
    let mut main_line = main_line(&client, &cargo, &dir);
    let boundary = stop_at(&client, &mut main_line, &["arc", "dbg_macro"]);
    let mut side = main_line.side_line(boundary);
    let written = "use std::sync::Arc;\n\nfn main() {\n    let _a = Arc::new(1);\n}\n";
    fs::write(dir.path().join("src/main.rs"), written).unwrap();
    client.push_chat(Ok(reply("review")));
    let Done::Passed { resolution, .. } = main_line.done(&mut side) else {
        panic!("/done did not pass");
    };
    pending(main_line.resolve(&resolution, &mut Refuse).unwrap());

    let withdrawal = main_line.withdraw();

    assert!(withdrawal.changes.is_empty(), "{:?}", withdrawal.changes);
    assert_eq!(
        fs::read_to_string(dir.path().join("src/main.rs")).unwrap(),
        written
    );
    assert_eq!(main_line.profile().status(&key("arc")), Status::Unknown);
}

#[test]
fn after_write_it_myself_and_a_later_side_line_the_end_of_the_turn_checks_the_build_again() {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = FakeCargo::new();
    for success in [true, false, true] {
        cargo.push(Ok(CargoOutput {
            success,
            stdout: String::new(),
            stderr: String::new(),
        }));
    }
    let mut main_line = main_line(&client, &cargo, &dir);
    let boundary = stop_at(&client, &mut main_line, &["arc", "dbg_macro"]);
    let mut arc = main_line.side_line(boundary);
    fs::write(
        dir.path().join("src/main.rs"),
        "use std::sync::Arc;\n\nfn main() {\n    let _a = Arc::new(1);\n}\n",
    )
    .unwrap();
    client.push_chat(Ok(reply("review")));
    let Done::Passed { resolution, .. } = main_line.done(&mut arc) else {
        panic!("/done did not pass");
    };
    let next = pending(main_line.resolve(&resolution, &mut Refuse).unwrap());
    // On the next side line the user writes lines that do not build, then chooses ① instead.
    let mut dbg = main_line.side_line(next);
    fs::write(dir.path().join("src/lib.rs"), "pub fn f() -> u8 { 1 }\n").unwrap();
    assert!(matches!(main_line.done(&mut dbg), Done::Failed(_)));
    let unlock = dbg
        .resolve(Choice::Unlock, &SessionLog::disabled())
        .unwrap();
    client.push_chat(Ok(reply("done")));

    let resumed = main_line.resolve(&unlock, &mut Refuse).unwrap().resumed();

    assert!(
        matches!(&resumed.turn.check, Some(Ok(output)) if output.success),
        "{:?}",
        resumed.turn.check
    );
    assert_eq!(
        cargo.calls().len(),
        3,
        "the end of the turn ran cargo check"
    );
}

#[test]
fn the_categories_left_get_their_side_lines_in_the_order_their_first_item_came() {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = cargo();
    let mut main_line = main_line(&client, &cargo, &dir);
    let boundary = stop_at(
        &client,
        &mut main_line,
        &["arc", "dbg_macro", "crate:rand", "mutex", "matches_macro"],
    );
    let mut opened = Vec::new();
    let mut next = boundary;

    for _ in 0..2 {
        let side = main_line.side_line(next);
        opened.push(side.boundary().items.clone());
        let unlock = side
            .resolve(Choice::Unlock, &SessionLog::disabled())
            .unwrap();
        next = pending(main_line.resolve(&unlock, &mut Refuse).unwrap());
    }
    opened.push(main_line.side_line(next).boundary().items.clone());

    assert_eq!(
        opened,
        [
            keys(&["arc", "mutex"]),
            keys(&["dbg_macro", "matches_macro"]),
            keys(&["crate:rand"]),
        ]
    );
}

#[test]
fn items_of_another_category_the_users_lines_used_in_write_it_myself_need_no_choice_of_their_own() {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = cargo();
    let mut main_line = main_line(&client, &cargo, &dir);
    let boundary = stop_at(&client, &mut main_line, &["arc", "dbg_macro"]);
    let mut side = main_line.side_line(boundary);
    fs::write(
        dir.path().join("src/main.rs"),
        "use std::sync::Arc;\n\nfn main() {\n    dbg!(Arc::new(1));\n}\n",
    )
    .unwrap();
    client.push_chat(Ok(reply("review")));
    let Done::Passed { resolution, .. } = main_line.done(&mut side) else {
        panic!("/done did not pass");
    };
    client.push_chat(Ok(reply("done")));

    let resumed = main_line
        .resolve(&resolution, &mut Refuse)
        .unwrap()
        .resumed();

    assert_eq!(resumed.chosen, [resolution]);
    for item in ["arc", "dbg_macro"] {
        assert_eq!(main_line.profile().status(&key(item)), Status::Known);
    }
}

#[test]
fn a_choice_for_an_item_of_a_bundle_that_recurred_after_rewrite_offers_no_rewrite() {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = cargo();
    let mut main_line = main_line(&client, &cargo, &dir);
    let boundary = stop_at(&client, &mut main_line, &["dbg_macro"]);
    client.push_chat(Ok(reply("policy")));
    let rewrite = choose(&main_line, boundary, Choice::Rewrite);
    // The model asks again for `dbg_macro`, now with `arc` before it.
    client.push_chat(Ok(ChatResponse {
        content: None,
        tool_calls: vec![ToolCall {
            id: "again".to_owned(),
            name: "request_boundary".to_owned(),
            arguments: json!({ "items": ["arc", "dbg_macro"], "reason": "needed" }).to_string(),
        }],
    }));
    let resumed = main_line.resolve(&rewrite, &mut Refuse).unwrap().resumed();
    let TurnEnd::Boundary(boundary) = resumed.turn.end else {
        panic!(
            "the turn did not stop at a boundary: {:?}",
            resumed.turn.end
        );
    };

    let arc = main_line.side_line(boundary.clone());
    assert_eq!(arc.boundary().items, keys(&["arc"]));
    assert!(!arc.is_recurrence(), "arc was not rewritten");
    let unlock = arc
        .resolve(Choice::Unlock, &SessionLog::disabled())
        .unwrap();
    let next = pending(main_line.resolve(&unlock, &mut Refuse).unwrap());
    let dbg = main_line.side_line(next);

    assert!(dbg.is_recurrence());
    assert_eq!(dbg.choices(), [Choice::Unlock, Choice::UserWrote]);
}

#[test]
fn each_choice_and_profile_change_is_logged_with_line_sub_and_the_main_line_events_are_not() {
    let dir = project();
    let logs = tempfile::tempdir().unwrap();
    let client = FakeChatClient::new();
    let cargo = cargo();
    let mut main_line =
        main_line(&client, &cargo, &dir).with_log(SessionLog::create(logs.path(), false).unwrap());
    let boundary = stop_at(&client, &mut main_line, &["arc", "dbg_macro", "mutex"]);
    let unlock = choose(&main_line, boundary, Choice::Unlock);
    let next = pending(main_line.resolve(&unlock, &mut Refuse).unwrap());
    client.push_chat(Ok(reply("Print with println! instead.")));
    let rewrite = choose(&main_line, next, Choice::Rewrite);
    client.push_chat(Ok(write_main("after", "fn main() {}\n")));
    client.push_chat(Ok(reply("done")));

    main_line.resolve(&rewrite, &mut Refuse).unwrap().resumed();

    let events = lines(main_line.log().path().unwrap());
    let kinds: Vec<(&str, Option<&str>)> = events
        .iter()
        .map(|event| (event["event"].as_str().unwrap(), event["line"].as_str()))
        .collect();
    assert_eq!(
        kinds,
        [
            ("tool_call", None),
            ("choice", Some("sub")),
            ("profile_change", Some("sub")),
            ("profile_change", Some("sub")),
            ("choice", Some("sub")),
            ("profile_change", Some("sub")),
            ("tool_call", None),
            ("cargo_check", None),
        ]
    );
    assert_eq!(
        events[1],
        json!({ "at": events[1]["at"], "line": "sub", "event": "choice", "choice": "unlock", "items": ["arc", "mutex"] })
    );
    assert_eq!(
        events[4],
        json!({ "at": events[4]["at"], "line": "sub", "event": "choice", "choice": "rewrite", "items": ["dbg_macro"], "policy": "Print with println! instead." })
    );
    let changes: Vec<(&str, &str, &str, &str)> = [&events[2], &events[3], &events[5]]
        .into_iter()
        .map(|event| {
            (
                event["item"].as_str().unwrap(),
                event["from"].as_str().unwrap(),
                event["to"].as_str().unwrap(),
                event["provenance"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        changes,
        [
            ("arc", "unknown", "known", "side_line"),
            ("mutex", "unknown", "known", "side_line"),
            ("dbg_macro", "unknown", "seen", "side_line"),
        ]
    );
}

#[test]
fn write_it_myself_is_logged_with_its_files_and_the_items_that_became_known_with_line_sub() {
    let dir = project();
    let logs = tempfile::tempdir().unwrap();
    let client = FakeChatClient::new();
    let cargo = cargo();
    let mut main_line =
        main_line(&client, &cargo, &dir).with_log(SessionLog::create(logs.path(), false).unwrap());
    let boundary = stop_at(&client, &mut main_line, &["dbg_macro"]);
    let mut side = main_line.side_line(boundary);
    fs::write(dir.path().join("src/main.rs"), WITH_DBG).unwrap();
    client.push_chat(Ok(reply("review")));
    let Done::Passed { resolution, .. } = main_line.done(&mut side) else {
        panic!("/done did not pass");
    };
    client.push_chat(Ok(reply("done")));

    main_line
        .resolve(&resolution, &mut Refuse)
        .unwrap()
        .resumed();

    let events = lines(main_line.log().path().unwrap());
    let sub: Vec<&Value> = events
        .iter()
        .filter(|event| event.get("line").is_some())
        .collect();
    assert_eq!(
        sub,
        [
            &json!({ "at": sub[0]["at"], "line": "sub", "event": "choice", "choice": "user_wrote", "items": ["dbg_macro"], "files": ["src/main.rs"] }),
            &json!({ "at": sub[1]["at"], "line": "sub", "event": "profile_change", "item": "dbg_macro", "from": "unknown", "to": "known", "provenance": "wrote_it" }),
        ]
    );
}

#[test]
fn a_choice_that_changes_no_status_logs_no_profile_change() {
    let dir = project();
    let logs = tempfile::tempdir().unwrap();
    let client = FakeChatClient::new();
    let cargo = cargo();
    let mut profile = Profile::new();
    profile
        .transition(
            key("dbg_macro"),
            Status::Seen,
            Provenance::Manual,
            crate::profile::store::Timestamp::now(),
            None,
        )
        .unwrap();
    let tools = Tools::new(Root::new(dir.path()).unwrap(), &cargo);
    let mut main_line = MainLine::new(&client, Router::new(&Config::default()), tools, profile)
        .with_log(SessionLog::create(logs.path(), false).unwrap());
    let boundary = stop_at(&client, &mut main_line, &["dbg_macro"]);
    client.push_chat(Ok(reply("policy")));
    let rewrite = choose(&main_line, boundary, Choice::Rewrite);
    client.push_chat(Ok(reply("done")));

    main_line.resolve(&rewrite, &mut Refuse).unwrap().resumed();

    let events = lines(main_line.log().path().unwrap());
    let kinds: Vec<&str> = events
        .iter()
        .map(|event| event["event"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["tool_call", "choice"]);
}

#[test]
fn the_request_bodies_of_the_side_line_carry_line_sub_and_those_of_the_main_line_do_not() {
    let dir = project();
    let logs = tempfile::tempdir().unwrap();
    let client = FakeChatClient::new();
    let cargo = cargo();
    let mut main_line =
        main_line(&client, &cargo, &dir).with_log(SessionLog::create(logs.path(), true).unwrap());
    let boundary = stop_at(&client, &mut main_line, &["dbg_macro"]);
    let mut side = main_line.side_line(boundary);
    client.push_chat(Ok(reply("explanation")));
    client.push_chat(Ok(reply("answer")));
    client.push_chat(Ok(reply("policy")));
    side.explain(main_line.log()).unwrap();
    side.ask("where does it print?", main_line.log()).unwrap();
    let rewrite = side.resolve(Choice::Rewrite, main_line.log()).unwrap();
    client.push_chat(Ok(reply("done")));

    main_line.resolve(&rewrite, &mut Refuse).unwrap().resumed();

    let bodies = lines(main_line.log().requests_path().unwrap());
    let sides: Vec<Option<&str>> = bodies.iter().map(|body| body["line"].as_str()).collect();
    assert_eq!(
        sides,
        [None, Some("sub"), Some("sub"), Some("sub"), None],
        "the turn, the explanation, the answer, the policy, the turn going on"
    );
}

#[test]
fn a_choice_dropped_by_withdrawing_the_request_is_not_logged() {
    let dir = project();
    let logs = tempfile::tempdir().unwrap();
    let client = FakeChatClient::new();
    let cargo = cargo();
    let mut main_line =
        main_line(&client, &cargo, &dir).with_log(SessionLog::create(logs.path(), false).unwrap());
    let boundary = stop_at(&client, &mut main_line, &["arc", "dbg_macro"]);
    let unlock = choose(&main_line, boundary, Choice::Unlock);
    pending(main_line.resolve(&unlock, &mut Refuse).unwrap());

    main_line.withdraw();

    let events = lines(main_line.log().path().unwrap());
    assert!(
        events.iter().all(|event| event.get("line").is_none()),
        "{events:?}"
    );
}
