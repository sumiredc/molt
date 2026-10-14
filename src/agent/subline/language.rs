//! Tests of what the side line gives the model to answer in the user's language (PRD §3
//! Language, §8.6): the request the user typed, quoted, and no user message naming the language.
//! Named so that `cargo test subline::language` selects them.

use std::fs;

use serde_json::json;
use tempfile::TempDir;

use super::*;
use crate::agent::mainline::{MainLine, TurnEnd};
use crate::agent::tools::{ApprovalRequest, Approver, Tools};
use crate::config::Config;
use crate::llm::{ChatRequest, ChatResponse, FakeChatClient, ToolCall};
use crate::profile::store::Profile;
use crate::workspace::cargo::{CargoOutput, FakeCargo};
use crate::workspace::path::Root;

const WITH_DBG: &str = "fn main() {\n    dbg!(1);\n}\n";

/// What the user typed on the main line.
const REQUEST: &str = "Zeig mir den Wert von eins beim Debuggen";

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

fn write_main(id: &str) -> ChatResponse {
    ChatResponse {
        content: None,
        tool_calls: vec![ToolCall {
            id: id.to_owned(),
            name: "write_file".to_owned(),
            arguments: json!({ "path": "src/main.rs", "content": WITH_DBG }).to_string(),
        }],
    }
}

fn reply(text: &str) -> ChatResponse {
    ChatResponse {
        content: Some(text.to_owned()),
        tool_calls: Vec::new(),
    }
}

/// Runs a turn on [`REQUEST`] that the model's two `dbg!` writes stop at a boundary.
fn stop_at_a_boundary(client: &FakeChatClient, main_line: &mut MainLine) -> Boundary {
    client.push_chat(Ok(write_main("first")));
    match main_line.turn(REQUEST, &mut Refuse).end {
        TurnEnd::Boundary(boundary) => boundary,
        end => panic!("the turn did not stop at a boundary: {end:?}"),
    }
}

/// The user messages of `request`, in order.
fn user_messages(request: &ChatRequest) -> Vec<&str> {
    request
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::User { content } => Some(content.as_str()),
            _ => None,
        })
        .collect()
}

/// Every request the side line sends, from two side lines at the same boundary: on the first, a
/// question before the explanation, the explanation, a question after it and the policy of ②;
/// on the second, the guidance of ③ before the explanation and the review on `/done`.
fn side_line_requests() -> Vec<ChatRequest> {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = cargo();
    let mut main_line = main_line(&client, &cargo, &dir);
    let boundary = stop_at_a_boundary(&client, &mut main_line);
    let main_requests = client.requests().len();
    let log = SessionLog::disabled();

    let mut side = main_line.side_line(boundary.clone());
    for text in ["answer", "explanation", "answer", "policy"] {
        client.push_chat(Ok(reply(text)));
    }
    side.ask("what is it?", &log).unwrap();
    side.explain(&log).unwrap();
    side.ask("where does it print?", &log).unwrap();
    side.resolve(Choice::Rewrite, main_line.tools(), &log)
        .unwrap();

    let mut side = main_line.side_line(boundary);
    main_line.take_back();
    side.start_writing(main_line.tools());
    client.push_chat(Ok(reply("guidance")));
    client.push_chat(Ok(reply("review")));
    side.guide(&log).unwrap();
    fs::write(dir.path().join("src/main.rs"), WITH_DBG).unwrap();
    let Done::Passed { .. } = main_line.done(&mut side) else {
        panic!("/done did not pass");
    };

    let requests = client.requests().split_off(main_requests);
    assert_eq!(requests.len(), 6, "{requests:?}");
    requests
}

#[test]
fn every_request_of_the_side_line_quotes_the_request_the_user_typed() {
    for request in side_line_requests() {
        assert!(
            user_messages(&request)
                .iter()
                .any(|content| content.contains(REQUEST)),
            "{:?}",
            request.messages
        );
    }
}

#[test]
fn only_the_system_prompt_of_a_side_line_request_speaks_of_the_language() {
    for request in side_line_requests() {
        for content in user_messages(&request) {
            assert!(!content.contains("language"), "{content}");
        }
        let Some(Message::System { content }) = request.messages.first() else {
            panic!("no system prompt: {:?}", request.messages);
        };
        assert!(
            content.contains("Answer in the language of the user's"),
            "{content}"
        );
    }
}

#[test]
fn a_side_line_opened_after_rewrite_in_the_same_turn_quotes_the_request_the_user_typed() {
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
    client.push_chat(Ok(write_main("third")));
    let resumed = main_line.resolve(&resolution, &mut Refuse).unwrap();
    let TurnEnd::Boundary(again) = resumed.turn.end else {
        panic!("the turn did not stop again: {:?}", resumed.turn.end);
    };
    let mut side = main_line.side_line(again);
    client.push_chat(Ok(reply("explanation")));

    side.explain(&SessionLog::disabled()).unwrap();

    let request = client.requests().pop().unwrap();
    let [content] = user_messages(&request)[..] else {
        panic!("not one user message: {:?}", request.messages);
    };
    assert!(content.contains(REQUEST), "{content}");
    assert!(!content.contains("BOUNDARY RESOLVED"), "{content}");
}
