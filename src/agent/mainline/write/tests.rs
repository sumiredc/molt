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
fn rejected_content_is_not_written_and_its_rejection_names_the_line_element_and_category() {
    let dir = project();
    let before = snapshot(dir.path());
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    client.push_chat(Ok(write_file("w", "src/main.rs", WITH_DBG)));
    client.push_chat(Ok(reply("cannot")));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

    let turn = main_line.turn("debug 1", &mut Refuse);

    assert_eq!(snapshot(dir.path()), before);
    assert!(turn.written.is_empty());
    let result = result_of(&main_line, "w");
    assert!(
        result.starts_with("rejected: src/main.rs was not written"),
        "{result}"
    );
    assert!(
        result.contains("- line 2: dbg_macro (macros) in `dbg!`"),
        "{result}"
    );
    assert!(
        result.contains("Rewrite the content without these items"),
        "{result}"
    );
    assert!(result.contains("`request_boundary`"), "{result}");
    assert!(matches!(turn.end, TurnEnd::Finished), "{:?}", turn.end);
}

#[test]
fn a_rewrite_that_is_clean_after_a_rejection_is_written() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    client.push_chat(Ok(write_file("first", "src/main.rs", WITH_DBG)));
    client.push_chat(Ok(write_file("second", "src/main.rs", WITHOUT_DBG)));
    client.push_chat(Ok(reply("done")));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

    let turn = main_line.turn("show 1", &mut Refuse);

    assert!(matches!(turn.end, TurnEnd::Finished), "{:?}", turn.end);
    assert_eq!(read(&dir, "src/main.rs"), WITHOUT_DBG);
}

#[test]
fn a_crate_added_to_cargo_toml_that_is_not_known_is_rejected() {
    let dir = project();
    let manifest = read(&dir, "Cargo.toml");
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    client.push_chat(Ok(write_file(
        "w",
        "Cargo.toml",
        &format!("{manifest}tokio = \"1\"\n"),
    )));
    client.push_chat(Ok(reply("cannot")));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

    main_line.turn("add tokio", &mut Refuse);

    assert_eq!(read(&dir, "Cargo.toml"), manifest);
    let result = result_of(&main_line, "w");
    assert!(
        result.contains("- line 5: crate:tokio (crates)"),
        "{result}"
    );
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
fn the_same_element_rejected_twice_is_a_boundary_and_the_files_are_unchanged() {
    let dir = project();
    let before = snapshot(dir.path());
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    client.push_chat(Ok(write_file("first", "src/main.rs", WITH_DBG)));
    let again = "fn helper() {\n    dbg!(2);\n}\n";
    client.push_chat(Ok(ChatResponse {
        content: None,
        tool_calls: vec![
            tool_call(
                "second",
                "write_file",
                json!({ "path": "src/helper.rs", "content": again }),
            ),
            tool_call(
                "after",
                "write_file",
                json!({ "path": "src/main.rs", "content": WITHOUT_DBG }),
            ),
        ],
    }));
    client.push_chat(Ok(reply("never sent")));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

    let turn = main_line.turn("debug 1", &mut Refuse);

    let TurnEnd::Boundary(boundary) = &turn.end else {
        panic!("{:?}", turn.end);
    };
    assert_eq!(boundary.items, [dbg_macro()]);
    assert_eq!(
        boundary.cause,
        BoundaryCause::RejectedTwice {
            path: "src/helper.rs".to_owned(),
            content: again.to_owned(),
        }
    );
    assert_eq!(snapshot(dir.path()), before);
    assert!(result_of(&main_line, "second").starts_with("rejected again:"));
    assert_eq!(result_of(&main_line, "after"), prompts::NOT_RUN);
    assert_eq!(client.requests().len(), 2, "no request after the boundary");
}

#[test]
fn a_second_rejection_of_other_items_is_not_a_boundary() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    client.push_chat(Ok(write_file("first", "src/main.rs", WITH_DBG)));
    client.push_chat(Ok(write_file(
        "second",
        "src/main.rs",
        "fn main() {\n    tokio::spawn(1);\n}\n",
    )));
    client.push_chat(Ok(reply("cannot")));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

    let turn = main_line.turn("debug 1", &mut Refuse);

    assert!(matches!(turn.end, TurnEnd::Finished), "{:?}", turn.end);
    assert!(result_of(&main_line, "second").starts_with("rejected: "));
}

#[test]
fn only_the_items_rejected_before_make_the_boundary() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    client.push_chat(Ok(write_file("first", "src/main.rs", WITH_DBG)));
    client.push_chat(Ok(write_file(
        "second",
        "src/main.rs",
        "fn main() {\n    dbg!(tokio::spawn(1));\n}\n",
    )));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

    let turn = main_line.turn("debug 1", &mut Refuse);

    let TurnEnd::Boundary(boundary) = &turn.end else {
        panic!("{:?}", turn.end);
    };
    assert_eq!(boundary.items, [dbg_macro()]);
}

#[test]
fn rejections_are_counted_again_from_zero_in_the_next_turn() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    client.push_chat(Ok(write_file("first", "src/main.rs", WITH_DBG)));
    client.push_chat(Ok(reply("cannot")));
    client.push_chat(Ok(write_file("second", "src/main.rs", WITH_DBG)));
    client.push_chat(Ok(reply("cannot either")));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

    main_line.turn("debug 1", &mut Refuse);
    let turn = main_line.turn("debug 1 again", &mut Refuse);

    assert!(matches!(turn.end, TurnEnd::Finished), "{:?}", turn.end);
    assert!(result_of(&main_line, "second").starts_with("rejected: "));
}

#[test]
fn request_boundary_stops_the_turn_at_a_boundary_and_the_files_are_unchanged() {
    let dir = project();
    let before = snapshot(dir.path());
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    client.push_chat(Ok(call(
        "b",
        "request_boundary",
        json!({
            "items": ["arc", "crate:tokio", "arc"],
            "reason": "shared state across tasks",
            "proposed_code": "let a = Arc::new(1);",
        }),
    )));
    client.push_chat(Ok(reply("never sent")));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

    let turn = main_line.turn("share it", &mut Refuse);

    let TurnEnd::Boundary(boundary) = &turn.end else {
        panic!("{:?}", turn.end);
    };
    assert_eq!(
        boundary.items,
        [
            ItemKey::Element(Element::Arc),
            ItemKey::Crate("tokio".to_owned())
        ]
    );
    assert_eq!(
        boundary.cause,
        BoundaryCause::Requested {
            reason: "shared state across tasks".to_owned(),
            proposed_code: Some("let a = Arc::new(1);".to_owned()),
        }
    );
    assert_eq!(snapshot(dir.path()), before);
    assert_eq!(result_of(&main_line, "b"), prompts::boundary_requested());
    assert_eq!(client.requests().len(), 1);
}

#[test]
fn request_boundary_with_an_item_that_is_not_one_is_an_error_and_the_turn_goes_on() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    client.push_chat(Ok(call(
        "b",
        "request_boundary",
        json!({ "items": ["Arc<T>"], "reason": "shared state" }),
    )));
    client.push_chat(Ok(reply("ok")));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

    let turn = main_line.turn("share it", &mut Refuse);

    assert!(matches!(turn.end, TurnEnd::Finished), "{:?}", turn.end);
    let result = result_of(&main_line, "b");
    assert!(
        result.contains("not an element name or crate:<name>: Arc<T>"),
        "{result}"
    );
}

#[test]
fn request_boundary_with_no_items_is_an_error_and_the_turn_goes_on() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    client.push_chat(Ok(call(
        "b",
        "request_boundary",
        json!({ "items": [], "reason": "hard" }),
    )));
    client.push_chat(Ok(reply("ok")));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

    let turn = main_line.turn("share it", &mut Refuse);

    assert!(matches!(turn.end, TurnEnd::Finished), "{:?}", turn.end);
    assert!(result_of(&main_line, "b").contains("items must name at least one item"));
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
fn calls_in_one_response_are_each_a_first_rejection_since_the_model_has_seen_none() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    client.push_chat(Ok(ChatResponse {
        content: None,
        tool_calls: vec![
            tool_call(
                "a",
                "write_file",
                json!({ "path": "src/main.rs", "content": WITH_DBG }),
            ),
            tool_call(
                "b",
                "write_file",
                json!({ "path": "src/b.rs", "content": "fn b() {\n    dbg!(2);\n}\n" }),
            ),
        ],
    }));
    client.push_chat(Ok(write_file("c", "src/main.rs", WITH_DBG)));
    let mut main_line = main_line(&client, &cargo, &dir, Profile::new());

    let turn = main_line.turn("debug 1", &mut Refuse);

    for id in ["a", "b"] {
        assert!(result_of(&main_line, id).starts_with("rejected: "), "{id}");
    }
    assert!(
        matches!(turn.end, TurnEnd::Boundary(_)),
        "the next response is the second rejection: {:?}",
        turn.end
    );
    assert_eq!(client.requests().len(), 2);
}

#[test]
fn request_boundary_on_items_the_profile_allows_is_not_a_boundary() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    client.push_chat(Ok(call(
        "b",
        "request_boundary",
        json!({ "items": ["dbg_macro"], "reason": "debugging" }),
    )));
    client.push_chat(Ok(reply("ok")));
    let mut main_line = main_line(&client, &cargo, &dir, known(Element::DbgMacro));

    let turn = main_line.turn("debug 1", &mut Refuse);

    assert!(matches!(turn.end, TurnEnd::Finished), "{:?}", turn.end);
    let result = result_of(&main_line, "b");
    assert!(
        result.starts_with("not a boundary: the user's profile already allows dbg_macro."),
        "{result}"
    );
}

#[test]
fn request_boundary_leaves_out_the_items_the_profile_allows() {
    let dir = project();
    let (client, cargo) = (FakeChatClient::new(), FakeCargo::new());
    client.push_chat(Ok(call(
        "b",
        "request_boundary",
        json!({ "items": ["dbg_macro", "arc"], "reason": "shared debugging" }),
    )));
    let mut main_line = main_line(&client, &cargo, &dir, known(Element::DbgMacro));

    let turn = main_line.turn("share it", &mut Refuse);

    let TurnEnd::Boundary(boundary) = &turn.end else {
        panic!("{:?}", turn.end);
    };
    assert_eq!(boundary.items, [ItemKey::Element(Element::Arc)]);
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
