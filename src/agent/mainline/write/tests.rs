use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde_json::json;
use tempfile::TempDir;

use super::super::{Message, Router, Tools};
use super::*;
use crate::agent::tools::{ApprovalRequest, Approver};
use crate::config::Config;
use crate::llm::{ChatResponse, FakeChatClient, ToolCall};
use crate::profile::element::Element;
use crate::profile::status::{Provenance, Status};
use crate::profile::store::{Profile, Timestamp};
use crate::workspace::cargo::FakeCargo;
use crate::workspace::path::Root;

struct Refuse;

impl Approver for Refuse {
    fn confirm(&mut self, _request: &ApprovalRequest) -> bool {
        false
    }
}

fn project() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"demo\"\n\n[dependencies]\n",
    )
    .unwrap();
    fs::create_dir(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
    dir
}

/// Every file under `dir` with its content.
fn snapshot(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in fs::read_dir(&current).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let relative = path.strip_prefix(dir).unwrap().display().to_string();
                files.insert(relative, fs::read(&path).unwrap());
            }
        }
    }
    files
}

fn read(dir: &TempDir, path: &str) -> String {
    fs::read_to_string(dir.path().join(path)).unwrap()
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

fn tool_call(id: &str, name: &str, arguments: Value) -> ToolCall {
    ToolCall {
        id: id.to_owned(),
        name: name.to_owned(),
        arguments: arguments.to_string(),
    }
}

fn call(id: &str, name: &str, arguments: Value) -> ChatResponse {
    ChatResponse {
        content: None,
        tool_calls: vec![tool_call(id, name, arguments)],
    }
}

fn write_file(id: &str, path: &str, content: &str) -> ChatResponse {
    call(
        id,
        "write_file",
        json!({ "path": path, "content": content }),
    )
}

fn reply(text: &str) -> ChatResponse {
    ChatResponse {
        content: Some(text.to_owned()),
        tool_calls: Vec::new(),
    }
}

fn result_of(main_line: &MainLine, id: &str) -> String {
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
}

fn known(element: Element) -> Profile {
    let mut profile = Profile::new();
    profile
        .transition(
            ItemKey::Element(element),
            Status::Known,
            Provenance::Manual,
            Timestamp::from_unix_seconds(0),
            None,
        )
        .unwrap();
    profile
}

/// Uses `dbg!`, which is not `known` in a new profile.
const WITH_DBG: &str = "fn main() {\n    dbg!(1);\n}\n";
const WITHOUT_DBG: &str = "fn main() {\n    let _ = 1;\n}\n";

fn dbg_macro() -> ItemKey {
    ItemKey::Element(Element::DbgMacro)
}

#[test]
fn clean_content_of_write_file_is_written_to_the_disk() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    client.push_chat(Ok(write_file("w", "src/main.rs", WITHOUT_DBG)));
    client.push_chat(Ok(reply("done")));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

    let turn = main_line.turn("print 1", &mut Refuse);

    assert!(matches!(turn.end, TurnEnd::Finished), "{:?}", turn.end);
    assert_eq!(read(&dir, "src/main.rs"), WITHOUT_DBG);
    assert_eq!(result_of(&main_line, "w"), "written: src/main.rs");
    assert_eq!(turn.written, ["src/main.rs"]);
}

#[test]
fn clean_content_of_edit_file_is_written_to_the_disk() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    client.push_chat(Ok(call(
        "e",
        "edit_file",
        json!({ "path": "src/main.rs", "old_string": "{}", "new_string": "{ let _x = 1; }" }),
    )));
    client.push_chat(Ok(reply("done")));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

    main_line.turn("add a binding", &mut Refuse);

    assert_eq!(read(&dir, "src/main.rs"), "fn main() { let _x = 1; }\n");
    assert_eq!(result_of(&main_line, "e"), "written: src/main.rs");
}

#[test]
fn a_known_element_is_written() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    client.push_chat(Ok(write_file("w", "src/main.rs", WITH_DBG)));
    client.push_chat(Ok(reply("done")));
    let mut main_line = main_line(&client, &cargo, &dir, known(Element::DbgMacro));

    main_line.turn("debug 1", &mut Refuse);

    assert_eq!(read(&dir, "src/main.rs"), WITH_DBG);
}

#[test]
fn existing_code_on_unchanged_lines_does_not_stop_a_write() {
    let dir = project();
    fs::write(dir.path().join("src/main.rs"), WITH_DBG).unwrap();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    let edited = format!("{WITH_DBG}fn helper() {{}}\n");
    client.push_chat(Ok(write_file("w", "src/main.rs", &edited)));
    client.push_chat(Ok(reply("done")));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

    main_line.turn("add a helper", &mut Refuse);

    assert_eq!(read(&dir, "src/main.rs"), edited);
}

#[test]
fn a_module_of_the_project_is_not_taken_for_a_crate() {
    let dir = project();
    fs::write(dir.path().join("src/util.rs"), "pub fn f() {}\n").unwrap();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    let content = "fn main() {\n    util::f();\n}\n";
    client.push_chat(Ok(write_file("w", "src/main.rs", content)));
    client.push_chat(Ok(reply("done")));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

    main_line.turn("call util", &mut Refuse);

    assert_eq!(read(&dir, "src/main.rs"), content);
}

#[test]
fn content_that_does_not_parse_is_returned_with_line_and_column_and_counts_as_a_compile_fix() {
    let dir = project();
    let before = snapshot(dir.path());
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    client.push_chat(Ok(write_file(
        "w",
        "src/main.rs",
        "fn main() {\n    let = 1;\n}\n",
    )));
    client.push_chat(Ok(write_file("again", "src/main.rs", "fn main( {}\n")));
    client.push_chat(Ok(reply("cannot")));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

    let turn = main_line.turn("bind 1", &mut Refuse);

    assert_eq!(snapshot(dir.path()), before);
    let result = result_of(&main_line, "w");
    assert!(
        result.starts_with(
            "parse error: src/main.rs was not written, because the content does not parse at line 2, column "
        ),
        "{result}"
    );
    assert!(
        result.contains("Fix the content and write the file again."),
        "{result}"
    );
    assert_eq!(turn.compile_fixes, 2);
    assert!(matches!(turn.end, TurnEnd::Finished), "{:?}", turn.end);
}

#[test]
fn a_file_the_checker_cannot_check_is_not_written_and_is_neither_a_rejection_nor_a_fix() {
    let dir = project();
    let before = snapshot(dir.path());
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    for id in ["first", "second"] {
        client.push_chat(Ok(write_file(id, "README.md", "# demo\n")));
    }
    client.push_chat(Ok(reply("cannot")));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

    let turn = main_line.turn("write a readme", &mut Refuse);

    assert_eq!(snapshot(dir.path()), before);
    assert!(matches!(turn.end, TurnEnd::Finished), "{:?}", turn.end);
    assert_eq!(turn.compile_fixes, 0);
    let result = result_of(&main_line, "second");
    assert!(
        result.starts_with("error: README.md cannot be written"),
        "{result}"
    );
}

#[test]
fn a_file_written_twice_is_listed_once_among_the_written_files() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    client.push_chat(Ok(write_file("a", "src/main.rs", WITHOUT_DBG)));
    client.push_chat(Ok(write_file("b", "src/lib.rs", "pub fn f() {}\n")));
    client.push_chat(Ok(write_file("c", "src/main.rs", "fn main() {}\n")));
    client.push_chat(Ok(reply("done")));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

    let turn = main_line.turn("write twice", &mut Refuse);

    assert_eq!(turn.written, ["src/main.rs", "src/lib.rs"]);
}

#[test]
fn a_module_written_earlier_in_the_turn_is_not_taken_for_a_crate() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    client.push_chat(Ok(write_file("a", "src/util.rs", "pub fn f() {}\n")));
    let content = "fn main() {\n    util::f();\n}\n";
    client.push_chat(Ok(write_file("b", "src/main.rs", content)));
    client.push_chat(Ok(reply("done")));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

    main_line.turn("add util", &mut Refuse);

    assert_eq!(result_of(&main_line, "b"), "written: src/main.rs");
    assert_eq!(read(&dir, "src/main.rs"), content);
}

/// `n` lines that each bind a value: syntax outside the element list.
fn plain_lines(n: usize) -> String {
    (0..n).map(|i| format!("    let _v{i} = {i};\n")).collect()
}

/// A write of more than a step holds: four items the user has not written, over several lines.
const TOO_BIG: &str = "fn main() {\n    let a = vec![1];\n    for x in &a {\n        dbg!(x);\n    }\n    println!(\"{a:?}\");\n}\n";

fn boundary_of(turn: &Turn) -> &Boundary {
    match &turn.end {
        TurnEnd::Boundary(boundary) => boundary,
        end => panic!("the turn did not stop at a boundary: {end:?}"),
    }
}

#[test]
fn content_with_an_item_that_is_not_known_is_written_as_a_step_and_the_turn_stops_after_it() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    client.push_chat(Ok(write_file("w", "src/main.rs", WITH_DBG)));
    client.push_chat(Ok(reply("never sent")));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

    let turn = main_line.turn("debug 1", &mut Refuse);

    let boundary = boundary_of(&turn);
    assert_eq!(boundary.items, [dbg_macro()]);
    assert_eq!(boundary.step.path, "src/main.rs");
    assert_eq!(boundary.step.before.as_deref(), Some("fn main() {}\n"));
    assert_eq!(boundary.step.after, WITH_DBG);
    assert_eq!(
        boundary.step.lines,
        [
            (1, "fn main() {".to_owned()),
            (2, "    dbg!(1);".to_owned()),
            (3, "}".to_owned())
        ]
    );
    assert_eq!(read(&dir, "src/main.rs"), WITH_DBG, "the step is on disk");
    assert_eq!(
        result_of(&main_line, "w"),
        prompts::step_written("src/main.rs")
    );
    assert_eq!(client.requests().len(), 1);
    assert!(cargo.calls().is_empty(), "no check at a boundary");
}

#[test]
fn a_crate_added_to_cargo_toml_that_is_not_known_is_a_step() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    let manifest = "[package]\nname = \"demo\"\n\n[dependencies]\nserde = \"1\"\n";
    client.push_chat(Ok(write_file("w", "Cargo.toml", manifest)));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

    let turn = main_line.turn("add serde", &mut Refuse);

    let boundary = boundary_of(&turn);
    assert_eq!(boundary.items, [ItemKey::Crate("serde".to_owned())]);
    assert_eq!(read(&dir, "Cargo.toml"), manifest);
}

#[test]
fn a_write_over_the_step_limit_is_not_written_and_is_sent_back_to_be_split() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    client.push_chat(Ok(write_file("big", "src/main.rs", TOO_BIG)));
    client.push_chat(Ok(reply("I will split it.")));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

    let turn = main_line.turn("print the list", &mut Refuse);

    assert!(matches!(turn.end, TurnEnd::Finished), "{:?}", turn.end);
    assert_eq!(read(&dir, "src/main.rs"), "fn main() {}\n", "not written");
    let result = result_of(&main_line, "big");
    assert!(
        result.starts_with("not written: src/main.rs changes 7 lines"),
        "{result}"
    );
    assert!(result.contains("Split it into smaller writes"), "{result}");
    assert!(
        !result.contains("without"),
        "it asks only for smaller steps: {result}"
    );
    assert!(turn.written.is_empty());
}

#[test]
fn thirty_changed_lines_are_a_step_and_thirty_one_are_split() {
    for (lines, step) in [(27, true), (28, false)] {
        let dir = project();
        let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
        // `fn main() {`, `dbg!`, the plain lines and `}`.
        let content = format!("fn main() {{\n    dbg!(1);\n{}}}\n", plain_lines(lines));
        client.push_chat(Ok(write_file("w", "src/main.rs", &content)));
        client.push_chat(Ok(reply("done")));
        let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

        let turn = main_line.turn("write it", &mut Refuse);

        assert_eq!(
            matches!(turn.end, TurnEnd::Boundary(_)),
            step,
            "{} changed lines: {:?}",
            lines + 3,
            turn.end
        );
    }
}

#[test]
fn three_items_are_a_step_and_four_are_split() {
    let three = "fn main() {\n    let a = vec![1];\n    dbg!(&a);\n}\n";
    let four = "fn main() {\n    let a = vec![1];\n    dbg!(&a);\n    println!(\"{a:?}\");\n}\n";
    for (content, step) in [(three, true), (four, false)] {
        let dir = project();
        let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
        client.push_chat(Ok(write_file("w", "src/main.rs", content)));
        client.push_chat(Ok(reply("done")));
        let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

        let turn = main_line.turn("write it", &mut Refuse);

        assert_eq!(matches!(turn.end, TurnEnd::Boundary(_)), step, "{content}");
    }
}

#[test]
fn a_write_that_changes_a_single_line_is_a_step_however_many_items_it_holds() {
    let dir = project();
    fs::write(
        dir.path().join("src/main.rs"),
        "fn main() {\n    let _ = 1;\n}\n",
    )
    .unwrap();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    let line =
        "fn main() {\n    let _ = dbg!(vec![1].iter().map(|x| x + 1).collect::<Vec<_>>());\n}\n";
    client.push_chat(Ok(write_file("w", "src/main.rs", line)));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

    let turn = main_line.turn("write it", &mut Refuse);

    let boundary = boundary_of(&turn);
    assert!(boundary.items.len() > 3, "{:?}", boundary.items);
    assert_eq!(boundary.step.lines.len(), 1);
}

#[test]
fn the_calls_after_a_split_request_in_the_same_response_are_not_run() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    client.push_chat(Ok(ChatResponse {
        content: None,
        tool_calls: vec![
            tool_call(
                "big",
                "write_file",
                json!({ "path": "src/main.rs", "content": TOO_BIG }),
            ),
            tool_call(
                "lib",
                "write_file",
                json!({ "path": "src/lib.rs", "content": "pub fn f() {}\n" }),
            ),
        ],
    }));
    client.push_chat(Ok(reply("done")));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

    main_line.turn("write it", &mut Refuse);

    assert_eq!(result_of(&main_line, "lib"), prompts::NOT_RUN_AFTER_SPLIT);
    assert!(!dir.path().join("src/lib.rs").exists());
}

#[test]
fn the_calls_after_a_step_in_the_same_response_are_not_run() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    client.push_chat(Ok(ChatResponse {
        content: None,
        tool_calls: vec![
            tool_call(
                "step",
                "write_file",
                json!({ "path": "src/main.rs", "content": WITH_DBG }),
            ),
            tool_call(
                "lib",
                "write_file",
                json!({ "path": "src/lib.rs", "content": "pub fn f() {}\n" }),
            ),
        ],
    }));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

    let turn = main_line.turn("write it", &mut Refuse);

    boundary_of(&turn);
    assert_eq!(result_of(&main_line, "lib"), prompts::NOT_RUN);
    assert!(!dir.path().join("src/lib.rs").exists());
}

#[test]
fn the_next_split_request_after_three_in_a_row_stops_the_turn_and_a_step_starts_the_count_again() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    for id in ["s1", "s2", "s3", "s4"] {
        client.push_chat(Ok(write_file(id, "src/main.rs", TOO_BIG)));
    }
    client.push_chat(Ok(reply("never sent")));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

    let turn = main_line.turn("write it", &mut Refuse);

    assert!(
        matches!(turn.end, TurnEnd::SplitsExhausted),
        "{:?}",
        turn.end
    );
    assert_eq!(client.requests().len(), 4);
    assert_eq!(
        result_of(&main_line, "s4"),
        prompts::split_over_budget("src/main.rs", SPLIT_BUDGET)
    );

    // A write within the limit in between starts the count again.
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    for id in ["s1", "s2", "s3"] {
        client.push_chat(Ok(write_file(id, "src/main.rs", TOO_BIG)));
    }
    client.push_chat(Ok(write_file("clean", "src/lib.rs", "pub fn f() {}\n")));
    client.push_chat(Ok(write_file("s4", "src/main.rs", TOO_BIG)));
    client.push_chat(Ok(reply("done")));
    let mut again = self::main_line(&client, &cargo, &dir, Profile::new());

    let turn = again.turn("write it", &mut Refuse);

    assert!(
        !matches!(turn.end, TurnEnd::SplitsExhausted),
        "{:?}",
        turn.end
    );
}

#[test]
fn a_boundary_starts_the_count_of_tool_calls_again() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    for i in 0..30 {
        client.push_chat(Ok(call(&format!("r{i}"), "list_files", json!({}))));
    }
    client.push_chat(Ok(write_file("step", "src/main.rs", WITH_DBG)));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());
    let turn = main_line.turn("look around", &mut Refuse);
    let boundary = boundary_of(&turn).clone();
    let side = main_line.side_line(boundary);
    let resolution = side
        .resolve(
            crate::agent::subline::Choice::Unlock,
            main_line.tools(),
            &crate::session::SessionLog::disabled(),
        )
        .unwrap();
    for i in 0..30 {
        client.push_chat(Ok(call(&format!("a{i}"), "list_files", json!({}))));
    }
    client.push_chat(Ok(reply("done")));

    let resumed = main_line.resolve(&resolution, &mut Refuse).unwrap();

    assert!(
        matches!(resumed.turn.end, TurnEnd::Finished),
        "{:?}",
        resumed.turn.end
    );
    assert_eq!(resumed.turn.tool_calls.len(), 61);
}

#[test]
fn taking_a_step_back_restores_the_file_and_drops_it_from_the_turn() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    client.push_chat(Ok(write_file(
        "step",
        "src/new.rs",
        "pub fn f() {\n    dbg!(1);\n}\n",
    )));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());
    boundary_of(&main_line.turn("debug", &mut Refuse));

    let taken = main_line.take_back().unwrap();

    assert_eq!(taken.path, "src/new.rs");
    assert!(taken.restored);
    assert!(
        !dir.path().join("src/new.rs").exists(),
        "the step created it"
    );
    assert_eq!(main_line.take_back(), None, "once");
    let withdrawal = main_line.withdraw();
    assert!(withdrawal.changes.is_empty());
    assert!(withdrawal.check.is_none());
}

#[test]
fn a_file_changed_after_its_step_is_left_as_it_is_when_the_step_is_taken_back() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    client.push_chat(Ok(write_file("step", "src/main.rs", WITH_DBG)));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());
    boundary_of(&main_line.turn("debug 1", &mut Refuse));
    fs::write(dir.path().join("src/main.rs"), "// mine\n").unwrap();

    let taken = main_line.take_back().unwrap();

    assert!(!taken.restored);
    assert_eq!(read(&dir, "src/main.rs"), "// mine\n");
}

#[test]
fn next_keeps_the_step_marks_an_unknown_item_seen_and_it_is_a_step_again_later() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    client.push_chat(Ok(write_file("step", "src/main.rs", WITH_DBG)));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());
    let boundary = boundary_of(&main_line.turn("debug 1", &mut Refuse)).clone();
    let side = main_line.side_line(boundary);
    let resolution = side
        .resolve(
            crate::agent::subline::Choice::Next,
            main_line.tools(),
            &crate::session::SessionLog::disabled(),
        )
        .unwrap();
    client.push_chat(Ok(reply("done")));

    let resumed = main_line.resolve(&resolution, &mut Refuse).unwrap();

    assert!(resumed.taken_back.is_none());
    assert_eq!(read(&dir, "src/main.rs"), WITH_DBG, "the step stays");
    let entry = main_line.profile().entry(&dbg_macro()).unwrap();
    assert_eq!(entry.status(), Status::Seen);
    assert_eq!(entry.provenance(), Provenance::SideLine);
    let message = client.requests().last().unwrap().messages.last().cloned();
    assert!(
        matches!(&message, Some(Message::User { content }) if content.starts_with("BOUNDARY RESOLVED: next\n")),
        "{message:?}"
    );
    client.push_chat(Ok(write_file(
        "again",
        "src/lib.rs",
        "pub fn g() {\n    dbg!(2);\n}\n",
    )));
    let turn = main_line.turn("debug 2", &mut Refuse);
    assert_eq!(
        boundary_of(&turn).items,
        [dbg_macro()],
        "seen is still a step"
    );
}

#[test]
fn a_crate_chosen_next_may_be_added_with_cargo_add() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    client.push_chat(Ok(write_file(
        "step",
        "src/main.rs",
        "fn main() {\n    let _ = serde::VALUE;\n}\n",
    )));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());
    let boundary = boundary_of(&main_line.turn("use serde", &mut Refuse)).clone();
    let side = main_line.side_line(boundary);
    let resolution = side
        .resolve(
            crate::agent::subline::Choice::Next,
            main_line.tools(),
            &crate::session::SessionLog::disabled(),
        )
        .unwrap();
    client.push_chat(Ok(call(
        "add",
        "run_cargo",
        json!({ "subcommand": "add", "args": ["serde"] }),
    )));
    client.push_chat(Ok(reply("done")));

    main_line.resolve(&resolution, &mut Refuse).unwrap();

    // Asked, and refused by the user, rather than refused before asking.
    assert_eq!(
        result_of(&main_line, "add"),
        prompts::tool_error(ToolError::NotApproved)
    );
}
