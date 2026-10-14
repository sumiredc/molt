use super::*;

fn chunk(delta: &str, finish: Option<&str>) -> String {
    let finish = match finish {
        Some(reason) => format!("\"{reason}\""),
        None => "null".to_owned(),
    };
    format!("data: {{\"choices\":[{{\"index\":0,\"delta\":{delta},\"finish_reason\":{finish}}}]}}")
}

fn feed_all(assembler: &mut Assembler, lines: &[String]) -> Vec<StreamEvent> {
    lines
        .iter()
        .flat_map(|line| assembler.feed(line).unwrap())
        .collect()
}

#[test]
fn text_is_handed_over_a_line_at_a_time_and_the_last_line_at_the_end() {
    let mut assembler = Assembler::new(None);
    let lines = [
        chunk(r#"{"role":"assistant"}"#, None),
        chunk(r#"{"content":"Hel"}"#, None),
        chunk(r#"{"content":"lo\nwor"}"#, None),
        chunk(r#"{"content":"ld"}"#, Some("stop")),
        "data: [DONE]".to_owned(),
    ];

    let events = feed_all(&mut assembler, &lines);

    assert_eq!(events, [StreamEvent::Line("Hello".to_owned())]);
    assert!(assembler.is_done());
    let (last, response) = assembler.finish().unwrap();
    assert_eq!(last, [StreamEvent::Line("world".to_owned())]);
    assert_eq!(response.content.as_deref(), Some("Hello\nworld"));
    assert!(response.tool_calls.is_empty());
}

#[test]
fn reasoning_is_handed_over_without_its_text() {
    let mut assembler = Assembler::new(None);
    let lines = [
        chunk(r#"{"reasoning_content":"let me think"}"#, None),
        chunk(r#"{"reasoning":"more"}"#, None),
        chunk(r#"{"reasoning_content":""}"#, None),
        chunk(r#"{"content":"OK"}"#, Some("stop")),
    ];

    let events = feed_all(&mut assembler, &lines);

    assert_eq!(events, [StreamEvent::Reasoning, StreamEvent::Reasoning]);
    let (_, response) = assembler.finish().unwrap();
    assert_eq!(response.content.as_deref(), Some("OK"));
}

#[test]
fn a_tool_call_is_put_together_from_its_pieces_by_index() {
    let mut assembler = Assembler::new(None);
    let lines = [
        chunk(
            r#"{"tool_calls":[{"index":0,"id":"call_a","type":"function","function":{"name":"read_file","arguments":""}}]}"#,
            None,
        ),
        chunk(
            r#"{"tool_calls":[{"index":1,"id":"call_b","function":{"name":"list_files","arguments":"{}"}}]}"#,
            None,
        ),
        chunk(
            r#"{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\":"}}]}"#,
            None,
        ),
        chunk(
            r#"{"tool_calls":[{"index":0,"function":{"arguments":"\"src/main.rs\"}"}}]}"#,
            Some("tool_calls"),
        ),
    ];

    feed_all(&mut assembler, &lines);
    let (_, response) = assembler.finish().unwrap();

    assert_eq!(
        response.tool_calls,
        [
            ToolCall {
                id: "call_a".to_owned(),
                name: "read_file".to_owned(),
                arguments: r#"{"path":"src/main.rs"}"#.to_owned(),
            },
            ToolCall {
                id: "call_b".to_owned(),
                name: "list_files".to_owned(),
                arguments: "{}".to_owned(),
            },
        ]
    );
    assert_eq!(response.content, None);
}

#[test]
fn a_tool_call_sent_whole_is_taken_as_it_is() {
    let mut assembler = Assembler::new(None);
    let lines = [chunk(
        r#"{"tool_calls":[{"index":0,"id":"c","function":{"name":"finish","arguments":{"summary":"done"}}}]}"#,
        Some("tool_calls"),
    )];

    feed_all(&mut assembler, &lines);
    let (_, response) = assembler.finish().unwrap();

    assert_eq!(response.tool_calls[0].arguments, r#"{"summary":"done"}"#);
}

#[test]
fn comments_and_empty_lines_hand_over_nothing() {
    let mut assembler = Assembler::new(None);

    assert!(assembler.feed(": ping").unwrap().is_empty());
    assert!(assembler.feed("").unwrap().is_empty());
    assert!(assembler.feed("event: message").unwrap().is_empty());
}

#[test]
fn a_stream_that_ends_before_its_finish_reason_is_a_failure() {
    let mut assembler = Assembler::new(None);
    feed_all(&mut assembler, &[chunk(r#"{"content":"half"}"#, None)]);

    let error = assembler.finish().unwrap_err();

    assert!(
        matches!(&error, ChatError::InvalidResponse(why) if why.contains("finish_reason")),
        "{error:?}"
    );
}

#[test]
fn the_api_key_is_hidden_in_the_lines_and_the_response_even_across_pieces() {
    const KEY: &str = "sk-stream-test";
    let mut assembler = Assembler::new(Some(KEY));
    let lines = [
        chunk(r#"{"content":"key: sk-str"}"#, None),
        chunk(r#"{"content":"eam-test\nnext"}"#, Some("stop")),
    ];

    let events = feed_all(&mut assembler, &lines);
    let (last, response) = assembler.finish().unwrap();

    assert_eq!(events, [StreamEvent::Line("key: <redacted>".to_owned())]);
    assert_eq!(last, [StreamEvent::Line("next".to_owned())]);
    assert_eq!(response.content.as_deref(), Some("key: <redacted>\nnext"));
}

#[test]
fn a_chunk_that_is_not_json_is_an_invalid_response_with_the_key_hidden() {
    let mut assembler = Assembler::new(Some("sk-x"));

    let error = assembler.feed("data: {sk-x").unwrap_err();

    assert!(matches!(&error, ChatError::InvalidResponse(_)), "{error:?}");
    assert!(!error.to_string().contains("sk-x"), "{error}");
}
