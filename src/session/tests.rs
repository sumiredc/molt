use std::collections::BTreeSet;
use std::ffi::OsString;

use serde_json::Value;

use super::*;
use crate::llm::{Message, ToolChoice};

fn files_in(dir: &Path) -> BTreeSet<String> {
    fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect()
}

fn lines(path: &Path) -> Vec<Value> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn request() -> ChatRequest {
    ChatRequest {
        model: "model".to_owned(),
        messages: vec![Message::User {
            content: "write a parser".to_owned(),
        }],
        tools: Vec::new(),
        tool_choice: Some(ToolChoice::Auto),
        response_format: None,
        reasoning_effort: None,
    }
}

fn env(value: Option<&str>) -> impl Fn(&str) -> Option<OsString> {
    let value = value.map(OsString::from);
    move |name| (name == MOLT_LOG).then(|| value.clone()).flatten()
}

#[test]
fn session_events_are_written_one_per_line_with_their_kind_and_no_line_key() {
    let dir = tempfile::tempdir().unwrap();
    let log = SessionLog::create(dir.path(), false).unwrap();
    log.record(&Event::ToolCall {
        name: "read_file".to_owned(),
        arguments: r#"{"path":"src/main.rs"}"#.to_owned(),
    });
    log.record(&Event::CargoCheck(CargoCheck::NotRun {
        error: "no cargo".to_owned(),
    }));

    let events = lines(log.path().unwrap());
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["event"], "tool_call");
    assert_eq!(events[0]["name"], "read_file");
    assert_eq!(events[0]["arguments"], r#"{"path":"src/main.rs"}"#);
    assert_eq!(events[1]["event"], "cargo_check");
    assert_eq!(events[1]["status"], "not_run");
    for event in &events {
        assert!(event.get("line").is_none(), "{event}");
        assert!(event["at"].as_str().unwrap().ends_with('Z'), "{event}");
    }
}

#[test]
fn session_without_molt_log_creates_no_file_for_the_request_bodies() {
    let dir = tempfile::tempdir().unwrap();
    let log = SessionLog::create(dir.path(), false).unwrap();
    log.record_request(&request());

    assert_eq!(log.requests_path(), None);
    let files = files_in(dir.path());
    assert_eq!(files.len(), 1, "{files:?}");
    assert!(
        files
            .iter()
            .all(|name| name.ends_with(".jsonl") && !name.ends_with(".requests.jsonl"))
    );
}

#[test]
fn session_with_molt_log_writes_each_request_body_to_a_separate_file() {
    let dir = tempfile::tempdir().unwrap();
    let log = SessionLog::create(dir.path(), true).unwrap();
    log.record_request(&request());

    let requests = log.requests_path().unwrap();
    assert_ne!(Some(requests), log.path());
    assert!(requests.to_str().unwrap().ends_with(".requests.jsonl"));
    let bodies = lines(requests);
    assert_eq!(bodies.len(), 1);
    assert_eq!(bodies[0]["model"], "model");
    assert_eq!(bodies[0]["messages"][0]["content"], "write a parser");
    assert!(
        lines(log.path().unwrap()).is_empty(),
        "no event was recorded"
    );
}

#[test]
fn session_molt_log_is_enabled_by_any_value_but_empty_and_zero() {
    assert!(!request_bodies_enabled(env(None)));
    assert!(!request_bodies_enabled(env(Some(""))));
    assert!(!request_bodies_enabled(env(Some("0"))));
    assert!(request_bodies_enabled(env(Some("1"))));
    assert!(request_bodies_enabled(env(Some("requests"))));
}

#[test]
fn session_logs_get_a_file_each_in_a_directory_created_for_them() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = dir.path().join("sessions");
    let first = SessionLog::create(&sessions, true).unwrap();
    let second = SessionLog::create(&sessions, true).unwrap();

    assert!(first.path().unwrap().starts_with(&sessions));
    assert_ne!(second.path(), first.path());
    assert_ne!(second.requests_path(), first.requests_path());
    assert_eq!(files_in(&sessions).len(), 4);
}

#[test]
fn a_disabled_session_log_writes_nothing() {
    let log = SessionLog::disabled();
    log.record(&Event::ToolCall {
        name: "finish".to_owned(),
        arguments: "{}".to_owned(),
    });
    assert_eq!(log.path(), None);
    assert!(log.take_error().is_none());
}

#[test]
fn a_session_log_that_cannot_be_written_keeps_the_first_error_and_stops_writing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("read-only.jsonl");
    fs::write(&path, "").unwrap();
    let log = SessionLog::unwritable(&path);
    let event = Event::ToolCall {
        name: "finish".to_owned(),
        arguments: "{}".to_owned(),
    };

    log.record(&event);
    let (failed, _) = log.take_error().expect("the write failed");
    assert_eq!(failed, path);
    log.record(&event);
    assert!(log.take_error().is_none(), "no more writes after the error");
}
