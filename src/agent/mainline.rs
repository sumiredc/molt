//! The main line (PRD UC2): the message list of the conversation with the user, and the loop
//! that sends it to the model and runs the tools the model calls until the turn ends.
//!
//! A turn ends when the model calls `finish` or answers without calling a tool, when it asks
//! for more tool calls than the budget (PRD §8.4), when its arguments still do not parse after
//! the one resend (PRD §11), when the chat API fails (UC2's exception flow), or at a boundary
//! (the write path, in `write`).

mod write;

use std::collections::BTreeSet;

use serde::Deserialize;
use serde_json::{Value, json};

use super::prompts;
use super::tools::{Approver, Tools};
use crate::llm::types::{check_tool_name, parse_arguments};
use crate::llm::{
    ChatClient, ChatError, ChatRequest, InvalidArguments, Message, Role, Router, ToolCall,
    ToolChoice, ToolDefinition,
};
use crate::profile::status::ItemKey;
use crate::profile::store::Profile;

/// Tool calls per turn (PRD §8.4). Every call counts, `finish` too, so that no call the model
/// repeats can keep a turn going.
pub const TOOL_CALL_BUDGET: usize = 40;

/// The tool that ends the turn. It is handled by the loop, not by [`Tools`].
const FINISH: &str = "finish";

/// The tools that write a file. The loop checks their content before [`Tools`] writes it.
const WRITE_FILE: &str = "write_file";
const EDIT_FILE: &str = "edit_file";

/// The tool with which the model stops at a boundary itself. It is handled by the loop.
const REQUEST_BOUNDARY: &str = "request_boundary";

/// The main line on one project.
pub struct MainLine<'a> {
    client: &'a dyn ChatClient,
    router: Router,
    tools: Tools<'a>,
    profile: Profile,
    definitions: Vec<ToolDefinition>,
    messages: Vec<Message>,
}

/// How one turn went.
#[derive(Debug)]
pub struct Turn {
    pub end: TurnEnd,
    /// The tools the model called in this turn, in order, counted against the budget. Calls
    /// with unknown names or broken arguments are included, and so is `finish`.
    pub tool_calls: Vec<String>,
    /// What the model said last: the summary passed to `finish`, or the text of its last reply.
    pub reply: Option<String>,
    /// The files written in this turn, as the model named them, each once, in the order first
    /// written.
    pub written: Vec<String>,
    /// The compile fixes the model was asked for in this turn (PRD §8.4): so far, once for
    /// each write whose content did not parse.
    pub compile_fixes: usize,
}

/// Where the main line stopped because the request needs items the profile does not allow
/// (PRD UC2). Nothing was written for it. The user chooses how to go on (UC3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Boundary {
    /// The items the request needs, each once.
    pub items: Vec<ItemKey>,
    pub cause: BoundaryCause,
}

/// What made the boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoundaryCause {
    /// Content using `items` was rejected again after the model had seen their rejection in the
    /// same turn. `content` is what the model tried to write to `path`.
    RejectedTwice { path: String, content: String },
    /// The model called `request_boundary`.
    Requested {
        reason: String,
        proposed_code: Option<String>,
    },
}

/// Why a turn ended.
#[derive(Debug)]
pub enum TurnEnd {
    /// The model called `finish`, or answered without calling a tool.
    Finished,
    /// The model asked for one more tool call than [`TOOL_CALL_BUDGET`]. That call was not run.
    OverBudget,
    /// The model sent arguments that are not a JSON object again, right after it was asked to
    /// resend.
    InvalidArguments(InvalidArguments),
    /// The chat API failed: authentication, rate limit, network, and so on.
    ApiFailed(ChatError),
    /// The request needs items the profile does not allow.
    Boundary(Boundary),
}

/// What to do after one tool call.
enum Next {
    Continue,
    Stop(TurnEnd),
}

/// What the loop keeps within a turn besides [`Turn`].
#[derive(Default)]
struct Progress {
    /// Whether the previous response had broken arguments and was asked to resend them.
    resend_asked: bool,
    /// Whether this response has broken arguments the model is asked to resend.
    broken: bool,
    /// The items rejected in earlier responses of this turn, in any file. The model has seen
    /// those rejections, so writing one of these items again means it could not avoid it.
    rejected: BTreeSet<ItemKey>,
    /// The items rejected in the response being run. Calls in one response are made before the
    /// model sees any of their results, so these count only from the next response on.
    rejected_now: BTreeSet<ItemKey>,
}

impl<'a> MainLine<'a> {
    /// A main line with only the system prompt in its message list. Requests go to the
    /// `generate` model of `router`, and what the model writes is checked against `profile`.
    pub fn new(
        client: &'a dyn ChatClient,
        router: Router,
        tools: Tools<'a>,
        profile: Profile,
    ) -> Self {
        Self {
            client,
            router,
            tools,
            profile,
            definitions: tool_definitions(),
            messages: vec![Message::System {
                content: prompts::MAIN_LINE_SYSTEM.to_owned(),
            }],
        }
    }

    /// The message list so far, starting with the system prompt.
    #[cfg(test)]
    fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// Adds `input` to the message list and runs the loop until the turn ends. `approver` is
    /// asked before the tools that need approval.
    ///
    /// Whatever ends the turn, every tool call of the model gets a tool result in the message
    /// list, so the next turn can be sent as it is.
    pub fn turn(&mut self, input: &str, approver: &mut dyn Approver) -> Turn {
        self.messages.push(Message::User {
            content: input.to_owned(),
        });
        let mut turn = Turn {
            end: TurnEnd::Finished,
            tool_calls: Vec::new(),
            reply: None,
            written: Vec::new(),
            compile_fixes: 0,
        };
        let mut progress = Progress::default();
        loop {
            let response = match self.client.chat(&self.request()) {
                Ok(response) => response,
                Err(error) => {
                    turn.end = TurnEnd::ApiFailed(error);
                    return turn;
                }
            };
            if let Some(content) = response.content.as_ref().filter(|c| !c.trim().is_empty()) {
                turn.reply = Some(content.clone());
            }
            self.messages.push(Message::Assistant {
                content: response.content,
                tool_calls: response.tool_calls.clone(),
            });
            if response.tool_calls.is_empty() {
                return turn;
            }

            let mut stop = None;
            progress.broken = false;
            for call in &response.tool_calls {
                let result = if stop.is_some() {
                    prompts::NOT_RUN.to_owned()
                } else {
                    let (result, next) = self.run_call(call, &mut turn, &mut progress, approver);
                    if let Next::Stop(end) = next {
                        stop = Some(end);
                    }
                    result
                };
                self.messages.push(Message::Tool {
                    tool_call_id: call.id.clone(),
                    content: result,
                });
            }
            if let Some(end) = stop {
                turn.end = end;
                return turn;
            }
            progress.resend_asked = progress.broken;
            let rejected_now = std::mem::take(&mut progress.rejected_now);
            progress.rejected.extend(rejected_now);
        }
    }

    /// Runs one tool call and returns its result for the model. `progress.broken` is set when
    /// its arguments are not a JSON object and the model is asked to resend them.
    fn run_call(
        &self,
        call: &ToolCall,
        turn: &mut Turn,
        progress: &mut Progress,
        approver: &mut dyn Approver,
    ) -> (String, Next) {
        if turn.tool_calls.len() == TOOL_CALL_BUDGET {
            return (
                prompts::over_budget(TOOL_CALL_BUDGET),
                Next::Stop(TurnEnd::OverBudget),
            );
        }
        turn.tool_calls.push(call.name.clone());
        if let Err(unknown) = check_tool_name(call, &self.definitions) {
            return (prompts::tool_error(unknown), Next::Continue);
        }
        let arguments = match parse_arguments(call) {
            Ok(arguments) => arguments,
            Err(error) if progress.resend_asked => {
                let result = prompts::tool_error(&error);
                return (result, Next::Stop(TurnEnd::InvalidArguments(error)));
            }
            Err(error) => {
                progress.broken = true;
                return (prompts::resend_arguments(error), Next::Continue);
            }
        };
        match call.name.as_str() {
            WRITE_FILE | EDIT_FILE => return self.write(&call.name, arguments, turn, progress),
            REQUEST_BOUNDARY => return write::request_boundary(&self.profile, arguments),
            _ => {}
        }
        if call.name == FINISH {
            return match serde_json::from_value::<FinishArgs>(Value::Object(arguments)) {
                Ok(args) => {
                    if let Some(summary) = args.summary.filter(|s| !s.trim().is_empty()) {
                        turn.reply = Some(summary);
                    }
                    (prompts::FINISHED.to_owned(), Next::Stop(TurnEnd::Finished))
                }
                Err(error) => (
                    prompts::tool_error(format!("invalid arguments for tool `{FINISH}`: {error}")),
                    Next::Continue,
                ),
            };
        }
        let result = match self.tools.dispatch(&call.name, &arguments, approver) {
            Ok(output) => output,
            Err(error) => prompts::tool_error(error),
        };
        (result, Next::Continue)
    }

    fn request(&self) -> ChatRequest {
        let mut request = self.router.request(Role::Generate, self.messages.clone());
        request.tools = self.definitions.clone();
        request.tool_choice = Some(ToolChoice::Auto);
        request
    }
}

/// `finish`: ends the turn.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FinishArgs {
    summary: Option<String>,
}

/// The function calling schema (PRD §8.4): the tools [`Tools::dispatch`] runs, and the ones the
/// loop runs itself: `write_file` and `edit_file` through the check, `request_boundary` and
/// `finish`.
fn tool_definitions() -> Vec<ToolDefinition> {
    let tool =
        |name: &str, description: &str, properties: Value, required: &[&str]| ToolDefinition {
            name: name.to_owned(),
            description: description.to_owned(),
            parameters: json!({
                "type": "object",
                "properties": properties,
                "required": required,
                "additionalProperties": false,
            }),
        };
    vec![
        tool(
            "list_files",
            prompts::LIST_FILES,
            json!({ "path": { "type": "string", "description": prompts::LIST_FILES_PATH } }),
            &[],
        ),
        tool(
            "read_file",
            prompts::READ_FILE,
            json!({ "path": { "type": "string", "description": prompts::READ_FILE_PATH } }),
            &["path"],
        ),
        tool(
            WRITE_FILE,
            prompts::WRITE_FILE,
            json!({
                "path": { "type": "string", "description": prompts::WRITE_FILE_PATH },
                "content": { "type": "string", "description": prompts::WRITE_FILE_CONTENT },
            }),
            &["path", "content"],
        ),
        tool(
            EDIT_FILE,
            prompts::EDIT_FILE,
            json!({
                "path": { "type": "string", "description": prompts::EDIT_FILE_PATH },
                "old_string": { "type": "string", "description": prompts::EDIT_FILE_OLD_STRING },
                "new_string": { "type": "string", "description": prompts::EDIT_FILE_NEW_STRING },
            }),
            &["path", "old_string", "new_string"],
        ),
        tool(
            "run_cargo",
            prompts::RUN_CARGO,
            json!({
                "subcommand": {
                    "type": "string",
                    "enum": ["check", "test", "clippy", "build", "fmt", "add"],
                    "description": prompts::RUN_CARGO_SUBCOMMAND,
                },
                "args": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": prompts::RUN_CARGO_ARGS,
                },
            }),
            &["subcommand"],
        ),
        tool("git_status", prompts::GIT_STATUS, json!({}), &[]),
        tool(
            "git_diff",
            prompts::GIT_DIFF,
            json!({ "staged": { "type": "boolean", "description": prompts::GIT_DIFF_STAGED } }),
            &[],
        ),
        tool(
            "git_commit",
            prompts::GIT_COMMIT,
            json!({ "message": { "type": "string", "description": prompts::GIT_COMMIT_MESSAGE } }),
            &["message"],
        ),
        tool(
            REQUEST_BOUNDARY,
            prompts::REQUEST_BOUNDARY,
            json!({
                "items": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": prompts::REQUEST_BOUNDARY_ITEMS,
                },
                "reason": { "type": "string", "description": prompts::REQUEST_BOUNDARY_REASON },
                "proposed_code": {
                    "type": "string",
                    "description": prompts::REQUEST_BOUNDARY_PROPOSED_CODE,
                },
            }),
            &["items", "reason"],
        ),
        tool(
            FINISH,
            prompts::FINISH,
            json!({ "summary": { "type": "string", "description": prompts::FINISH_SUMMARY } }),
            &[],
        ),
    ]
}

/// Tests of one turn of the loop. The module is named so that `cargo test mainline::turn`
/// selects them.
#[cfg(test)]
mod turn {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::Path;

    use serde_json::Map;
    use tempfile::TempDir;

    use super::*;
    use crate::agent::tools::{ApprovalRequest, ToolError};
    use crate::config::Config;
    use crate::llm::{ChatResponse, FakeChatClient};
    use crate::workspace::cargo::{CargoCall, CargoOutput, FakeCargo, Subcommand};
    use crate::workspace::path::Root;

    /// Answers no to every approval.
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

    fn main_line<'a>(
        client: &'a FakeChatClient,
        cargo: &'a FakeCargo,
        dir: &TempDir,
    ) -> MainLine<'a> {
        let tools = Tools::new(Root::new(dir.path()).unwrap(), cargo);
        MainLine::new(
            client,
            Router::new(&Config::default()),
            tools,
            Profile::new(),
        )
    }

    fn call(id: &str, name: &str, arguments: &str) -> ToolCall {
        ToolCall {
            id: id.to_owned(),
            name: name.to_owned(),
            arguments: arguments.to_owned(),
        }
    }

    fn calls(tool_calls: Vec<ToolCall>) -> ChatResponse {
        ChatResponse {
            content: None,
            tool_calls,
        }
    }

    fn reply(text: &str) -> ChatResponse {
        ChatResponse {
            content: Some(text.to_owned()),
            tool_calls: Vec::new(),
        }
    }

    fn cargo_check(id: &str) -> ChatResponse {
        calls(vec![call(id, "run_cargo", r#"{"subcommand": "check"}"#)])
    }

    fn passed() -> CargoOutput {
        CargoOutput {
            success: true,
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    /// The tool results in the message list, by tool call id.
    fn results(main_line: &MainLine) -> Vec<(String, String)> {
        main_line
            .messages()
            .iter()
            .filter_map(|message| match message {
                Message::Tool {
                    tool_call_id,
                    content,
                } => Some((tool_call_id.clone(), content.clone())),
                _ => None,
            })
            .collect()
    }

    fn result_of(main_line: &MainLine, id: &str) -> String {
        results(main_line)
            .into_iter()
            .find(|(call_id, _)| call_id == id)
            .map(|(_, content)| content)
            .unwrap_or_else(|| panic!("no tool result for {id}"))
    }

    #[test]
    fn the_41st_tool_call_is_not_run_and_the_turn_stops_with_the_calls_so_far() {
        let dir = project();
        let client = FakeChatClient::new();
        let cargo = FakeCargo::new();
        for i in 0..=TOOL_CALL_BUDGET {
            client.push_chat(Ok(cargo_check(&format!("call_{i}"))));
            cargo.push(Ok(passed()));
        }
        client.push_chat(Ok(reply("never sent")));
        let mut main_line = main_line(&client, &cargo, &dir);

        let turn = main_line.turn("check it many times", &mut Refuse);

        assert!(matches!(turn.end, TurnEnd::OverBudget), "{:?}", turn.end);
        assert_eq!(cargo.calls(), vec![CargoCall::Run(Subcommand::Check); 40]);
        assert_eq!(turn.tool_calls, vec!["run_cargo"; 40]);
        assert_eq!(client.requests().len(), 41, "no request after the stop");
        assert_eq!(
            result_of(&main_line, "call_40"),
            prompts::over_budget(TOOL_CALL_BUDGET)
        );
    }

    #[test]
    fn the_40th_tool_call_is_still_run() {
        let dir = project();
        let client = FakeChatClient::new();
        let cargo = FakeCargo::new();
        for i in 0..TOOL_CALL_BUDGET {
            client.push_chat(Ok(cargo_check(&format!("call_{i}"))));
            cargo.push(Ok(passed()));
        }
        client.push_chat(Ok(reply("checked 40 times")));
        let mut main_line = main_line(&client, &cargo, &dir);

        let turn = main_line.turn("check it many times", &mut Refuse);

        assert!(matches!(turn.end, TurnEnd::Finished), "{:?}", turn.end);
        assert_eq!(cargo.calls().len(), 40);
        assert_eq!(turn.reply.as_deref(), Some("checked 40 times"));
    }

    #[test]
    fn calls_over_the_budget_in_one_response_are_not_run_and_each_gets_a_result() {
        let dir = project();
        let client = FakeChatClient::new();
        let cargo = FakeCargo::new();
        for i in 0..TOOL_CALL_BUDGET - 1 {
            client.push_chat(Ok(cargo_check(&format!("call_{i}"))));
        }
        client.push_chat(Ok(calls(vec![
            call("last", "run_cargo", r#"{"subcommand": "check"}"#),
            call("over", "run_cargo", r#"{"subcommand": "check"}"#),
            call("after", "run_cargo", r#"{"subcommand": "check"}"#),
        ])));
        for _ in 0..TOOL_CALL_BUDGET {
            cargo.push(Ok(passed()));
        }
        let mut main_line = main_line(&client, &cargo, &dir);

        let turn = main_line.turn("check it many times", &mut Refuse);

        assert!(matches!(turn.end, TurnEnd::OverBudget), "{:?}", turn.end);
        assert_eq!(cargo.calls().len(), 40);
        assert_eq!(
            result_of(&main_line, "over"),
            prompts::over_budget(TOOL_CALL_BUDGET)
        );
        assert_eq!(result_of(&main_line, "after"), prompts::NOT_RUN);
    }

    #[test]
    fn a_finish_after_40_tool_calls_is_over_the_budget_too() {
        let dir = project();
        let client = FakeChatClient::new();
        let cargo = FakeCargo::new();
        for i in 0..TOOL_CALL_BUDGET {
            client.push_chat(Ok(cargo_check(&format!("call_{i}"))));
            cargo.push(Ok(passed()));
        }
        client.push_chat(Ok(calls(vec![call(
            "done",
            "finish",
            r#"{"summary": "checked"}"#,
        )])));
        let mut main_line = main_line(&client, &cargo, &dir);

        let turn = main_line.turn("check it many times", &mut Refuse);

        assert!(matches!(turn.end, TurnEnd::OverBudget), "{:?}", turn.end);
        assert_eq!(turn.reply, None);
        assert_eq!(
            result_of(&main_line, "done"),
            prompts::over_budget(TOOL_CALL_BUDGET)
        );
    }

    #[test]
    fn a_finish_repeated_with_arguments_that_do_not_fit_still_stops_at_the_budget() {
        let dir = project();
        let client = FakeChatClient::new();
        let cargo = FakeCargo::new();
        for i in 0..=TOOL_CALL_BUDGET {
            client.push_chat(Ok(calls(vec![call(
                &format!("call_{i}"),
                "finish",
                r#"{"summary": 1}"#,
            )])));
        }
        client.push_chat(Ok(reply("never sent")));
        let mut main_line = main_line(&client, &cargo, &dir);

        let turn = main_line.turn("finish", &mut Refuse);

        assert!(matches!(turn.end, TurnEnd::OverBudget), "{:?}", turn.end);
        assert_eq!(client.requests().len(), 41);
    }

    #[test]
    fn broken_arguments_after_a_call_that_parsed_get_a_resend_request_again() {
        let dir = project();
        let client = FakeChatClient::new();
        let cargo = FakeCargo::new();
        client.push_chat(Ok(calls(vec![call("first", "read_file", "{")])));
        client.push_chat(Ok(calls(vec![call(
            "fixed",
            "read_file",
            r#"{"path": "src/main.rs"}"#,
        )])));
        client.push_chat(Ok(calls(vec![call("later", "read_file", "{")])));
        client.push_chat(Ok(reply("done")));
        let mut main_line = main_line(&client, &cargo, &dir);

        let turn = main_line.turn("read main", &mut Refuse);

        assert!(matches!(turn.end, TurnEnd::Finished), "{:?}", turn.end);
        assert!(result_of(&main_line, "later").contains("Send this tool call again"));
    }

    #[test]
    fn every_broken_call_in_one_response_gets_the_resend_request_and_the_turn_goes_on() {
        let dir = project();
        let client = FakeChatClient::new();
        let cargo = FakeCargo::new();
        client.push_chat(Ok(calls(vec![
            call("a", "read_file", "{"),
            call("b", "list_files", "["),
        ])));
        client.push_chat(Ok(reply("done")));
        let mut main_line = main_line(&client, &cargo, &dir);

        let turn = main_line.turn("look around", &mut Refuse);

        assert!(matches!(turn.end, TurnEnd::Finished), "{:?}", turn.end);
        for id in ["a", "b"] {
            assert!(result_of(&main_line, id).contains("Send this tool call again"));
        }
    }

    #[test]
    fn the_system_prompt_tells_the_model_to_answer_in_the_language_of_the_latest_input() {
        let dir = project();
        let client = FakeChatClient::new();
        let cargo = FakeCargo::new();
        client.push_chat(Ok(reply("こんにちは")));
        let mut main_line = main_line(&client, &cargo, &dir);

        main_line.turn("こんにちは", &mut Refuse);

        let requests = client.requests();
        let Message::System { content } = &requests[0].messages[0] else {
            panic!("the first message is not the system prompt: {requests:?}");
        };
        assert!(
            content.contains("Answer in the language of the user's latest input."),
            "{content}"
        );
    }

    #[test]
    fn an_api_error_ends_the_turn_with_its_cause_and_leaves_the_files_unchanged() {
        let dir = project();
        let before = snapshot(dir.path());
        let client = FakeChatClient::new();
        let cargo = FakeCargo::new();
        client.push_chat(Ok(calls(vec![call(
            "call_0",
            "read_file",
            r#"{"path": "src/main.rs"}"#,
        )])));
        client.push_chat(Err(ChatError::Status {
            status: 429,
            body: "rate limited".to_owned(),
        }));
        let mut main_line = main_line(&client, &cargo, &dir);

        let turn = main_line.turn("rewrite main", &mut Refuse);

        let TurnEnd::ApiFailed(error) = &turn.end else {
            panic!("{:?}", turn.end);
        };
        assert!(error.to_string().contains("HTTP 429"), "{error}");
        assert_eq!(snapshot(dir.path()), before);
        assert!(cargo.calls().is_empty());
    }

    #[test]
    fn broken_arguments_get_one_resend_request_and_broken_ones_again_stop_the_turn() {
        let dir = project();
        let client = FakeChatClient::new();
        let cargo = FakeCargo::new();
        client.push_chat(Ok(calls(vec![call("first", "read_file", r#"{"path": "#)])));
        client.push_chat(Ok(calls(vec![call("second", "read_file", r#"{"path"#)])));
        client.push_chat(Ok(reply("never sent")));
        let mut main_line = main_line(&client, &cargo, &dir);

        let turn = main_line.turn("read main", &mut Refuse);

        let TurnEnd::InvalidArguments(error) = &turn.end else {
            panic!("{:?}", turn.end);
        };
        assert_eq!(error.id, "second");
        let resend_requests = results(&main_line)
            .into_iter()
            .filter(|(_, content)| content.contains("Send this tool call again"))
            .count();
        assert_eq!(resend_requests, 1);
        assert!(result_of(&main_line, "first").contains("Send this tool call again"));
        assert_eq!(client.requests().len(), 2, "no request after the stop");
    }

    #[test]
    fn a_resent_call_that_parses_is_run_and_the_turn_goes_on() {
        let dir = project();
        let client = FakeChatClient::new();
        let cargo = FakeCargo::new();
        client.push_chat(Ok(calls(vec![call("first", "read_file", r#"{"path": "#)])));
        client.push_chat(Ok(calls(vec![call(
            "second",
            "read_file",
            r#"{"path": "src/main.rs"}"#,
        )])));
        client.push_chat(Ok(reply("main is empty")));
        let mut main_line = main_line(&client, &cargo, &dir);

        let turn = main_line.turn("read main", &mut Refuse);

        assert!(matches!(turn.end, TurnEnd::Finished), "{:?}", turn.end);
        assert_eq!(result_of(&main_line, "second"), "fn main() {}\n");
        assert_eq!(turn.reply.as_deref(), Some("main is empty"));
    }

    #[test]
    fn an_unknown_tool_gets_the_valid_names_as_its_result_and_the_turn_goes_on() {
        let dir = project();
        let client = FakeChatClient::new();
        let cargo = FakeCargo::new();
        client.push_chat(Ok(calls(vec![call(
            "call_0",
            "delete_file",
            r#"{"path": "src/main.rs"}"#,
        )])));
        client.push_chat(Ok(reply("cannot delete")));
        let mut main_line = main_line(&client, &cargo, &dir);

        let turn = main_line.turn("delete main", &mut Refuse);

        let result = result_of(&main_line, "call_0");
        assert!(result.contains("unknown tool `delete_file`"), "{result}");
        for name in [
            "list_files",
            "read_file",
            "write_file",
            "edit_file",
            "run_cargo",
            "git_status",
            "git_diff",
            "git_commit",
            "request_boundary",
            "finish",
        ] {
            assert!(result.contains(name), "{result}");
        }
        assert!(matches!(turn.end, TurnEnd::Finished), "{:?}", turn.end);
        assert!(dir.path().join("src/main.rs").exists());
    }

    #[test]
    fn finish_ends_the_turn_with_its_summary_and_later_calls_are_not_run() {
        let dir = project();
        let client = FakeChatClient::new();
        let cargo = FakeCargo::new();
        client.push_chat(Ok(calls(vec![
            call("done", "finish", r#"{"summary": "nothing to do"}"#),
            call("after", "run_cargo", r#"{"subcommand": "check"}"#),
        ])));
        let mut main_line = main_line(&client, &cargo, &dir);

        let turn = main_line.turn("anything?", &mut Refuse);

        assert!(matches!(turn.end, TurnEnd::Finished), "{:?}", turn.end);
        assert_eq!(turn.reply.as_deref(), Some("nothing to do"));
        assert_eq!(result_of(&main_line, "after"), prompts::NOT_RUN);
        assert!(cargo.calls().is_empty());
        assert_eq!(client.requests().len(), 1);
    }

    #[test]
    fn a_reply_without_tool_calls_ends_the_turn_with_the_reply() {
        let dir = project();
        let client = FakeChatClient::new();
        let cargo = FakeCargo::new();
        client.push_chat(Ok(reply("a slice is a view into a sequence")));
        let mut main_line = main_line(&client, &cargo, &dir);

        let turn = main_line.turn("what is a slice?", &mut Refuse);

        assert!(matches!(turn.end, TurnEnd::Finished), "{:?}", turn.end);
        assert_eq!(
            turn.reply.as_deref(),
            Some("a slice is a view into a sequence")
        );
        assert!(turn.tool_calls.is_empty());
    }

    #[test]
    fn a_tool_error_is_returned_to_the_model_and_the_turn_goes_on() {
        let dir = project();
        let client = FakeChatClient::new();
        let cargo = FakeCargo::new();
        client.push_chat(Ok(calls(vec![call(
            "call_0",
            "read_file",
            r#"{"path": "missing.rs"}"#,
        )])));
        client.push_chat(Ok(reply("no such file")));
        let mut main_line = main_line(&client, &cargo, &dir);

        let turn = main_line.turn("read missing.rs", &mut Refuse);

        assert!(result_of(&main_line, "call_0").starts_with("error: missing.rs"));
        assert!(matches!(turn.end, TurnEnd::Finished), "{:?}", turn.end);
    }

    #[test]
    fn the_next_turn_sends_the_whole_message_list_so_far() {
        let dir = project();
        let client = FakeChatClient::new();
        let cargo = FakeCargo::new();
        client.push_chat(Ok(reply("first answer")));
        client.push_chat(Ok(reply("second answer")));
        let mut main_line = main_line(&client, &cargo, &dir);

        main_line.turn("first", &mut Refuse);
        main_line.turn("second", &mut Refuse);

        let sent = &client.requests()[1].messages;
        assert_eq!(sent.len(), 4, "{sent:?}");
        assert_eq!(
            sent[1],
            Message::User {
                content: "first".to_owned()
            }
        );
        assert_eq!(
            sent[3],
            Message::User {
                content: "second".to_owned()
            }
        );
    }

    #[test]
    fn the_request_offers_every_tool_of_the_write_path_to_the_generate_model() {
        let dir = project();
        let client = FakeChatClient::new();
        let cargo = FakeCargo::new();
        client.push_chat(Ok(reply("ok")));
        let mut main_line = main_line(&client, &cargo, &dir);

        main_line.turn("hello", &mut Refuse);

        let request = &client.requests()[0];
        let names: Vec<_> = request
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect();
        assert_eq!(
            names,
            [
                "list_files",
                "read_file",
                "write_file",
                "edit_file",
                "run_cargo",
                "git_status",
                "git_diff",
                "git_commit",
                "request_boundary",
                "finish"
            ]
        );
        assert_eq!(request.model, Config::default().models.generate);
        assert_eq!(request.tool_choice, Some(ToolChoice::Auto));
    }

    #[test]
    fn every_offered_tool_the_loop_does_not_run_itself_is_one_the_tools_dispatch() {
        let dir = project();
        let cargo = FakeCargo::new();
        let tools = Tools::new(Root::new(dir.path()).unwrap(), &cargo);
        for definition in tool_definitions() {
            if [FINISH, WRITE_FILE, EDIT_FILE, REQUEST_BOUNDARY].contains(&definition.name.as_str())
            {
                continue;
            }
            let result = tools.dispatch(&definition.name, &Map::new(), &mut Refuse);
            assert!(
                !matches!(result, Err(ToolError::UnknownTool(_))),
                "{}",
                definition.name
            );
        }
    }
}
