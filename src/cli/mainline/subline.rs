//! The side line on the terminal (PRD UC3, §8.6): a ruled banner on entering and leaving, its own
//! prompt `molt [sub: arc, mutex]> `, and its output indented by 2 spaces.

mod resolve;

use super::show_error_of;
use crate::agent::subline::{Choice, SubLine};
use crate::cli::ui::{Input, Ui};
use crate::llm::ChatError;
use crate::profile::status::ItemKey;
use crate::session::SessionLog;

/// The width of the banners, in characters.
const BANNER_WIDTH: usize = 60;

/// What the user can do on the side line (UC3).
const HINT: &str = "Ask anything about it, as many times as you like. Type /choose to choose how to go on, \
     or /abort to withdraw the request.";

/// How the user left the side line.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Left {
    /// `/choose`, and the user chose one.
    Chose(Choice),
    /// `/abort`: the request is to be withdrawn.
    Aborted,
    /// Ctrl-C, at the prompt or while waiting for the model: the main line goes back to being
    /// paused at the boundary.
    Interrupted,
    /// Ctrl-D: the main line ends.
    Eof,
    /// The [`Ui`] could not read a line. The error was printed.
    Failed,
}

/// Runs the side line until the user leaves it. The explanation is asked for the first time only;
/// when the side line is opened again, the questions go on where they were.
pub(super) fn run(ui: &mut dyn Ui, side: &mut SubLine, log: &SessionLog) -> Left {
    ui.show(&banner(&format!(
        "side line: {}",
        names(&side.boundary().items)
    )));
    if !side.explained() {
        match side.explain(log) {
            Ok(explanation) => ui.show(&indent(&explanation)),
            Err(ChatError::Interrupted) => {
                show_error_of(ui, log);
                return leave(ui, Left::Interrupted);
            }
            Err(error) => ui.show(&indent(&format!("Could not get the explanation: {error}"))),
        }
        show_error_of(ui, log);
    }
    if side.is_recurrence() {
        ui.show(&indent(&resolve::recurred(&side.boundary().items)));
    }
    ui.show(&indent(HINT));
    let prompt = prompt(&side.boundary().items);
    let left = loop {
        match ui.read_line(&prompt) {
            Ok(Input::Line(line)) => {
                let line = line.trim();
                if line == "/abort" {
                    break Left::Aborted;
                } else if line == "/choose" {
                    match resolve::choose(ui, side) {
                        Ok(Some(choice)) => break Left::Chose(choice),
                        Ok(None) => ui.show(&indent(HINT)),
                        Err(error) => {
                            eprintln!("{error}");
                            return Left::Failed;
                        }
                    }
                } else if line.is_empty() {
                } else if line.starts_with('/') {
                    ui.show(&indent(&format!("Unknown command {line}. {HINT}")));
                } else {
                    match side.ask(line, log) {
                        Ok(answer) => ui.show(&indent(&answer)),
                        Err(ChatError::Interrupted) => {
                            show_error_of(ui, log);
                            break Left::Interrupted;
                        }
                        Err(error) => {
                            ui.show(&indent(&format!("Could not get an answer: {error}")))
                        }
                    }
                    show_error_of(ui, log);
                }
            }
            Ok(Input::Interrupted) => break Left::Interrupted,
            Ok(Input::Eof) => break Left::Eof,
            Err(error) => {
                eprintln!("{error}");
                return Left::Failed;
            }
        }
    };
    leave(ui, left)
}

/// Shows the banner of leaving the side line, and returns `left`.
fn leave(ui: &mut dyn Ui, left: Left) -> Left {
    ui.show(&banner("back to the main line"));
    left
}

/// The side-line prompt (PRD §8.6), e.g. `molt [sub: arc, mutex]> `.
fn prompt(items: &[ItemKey]) -> String {
    format!("molt [sub: {}]> ", names(items))
}

fn names(items: &[ItemKey]) -> String {
    items
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// `── title ──────…`, ruled out to [`BANNER_WIDTH`].
fn banner(title: &str) -> String {
    let rule = BANNER_WIDTH
        .saturating_sub(title.chars().count() + 4)
        .max(3);
    format!("── {title} {}", "─".repeat(rule))
}

/// `text` with every line that is not blank indented by 2 spaces. A blank line stays empty.
fn indent(text: &str) -> String {
    text.lines()
        .map(|line| {
            if line.trim().is_empty() {
                String::new()
            } else {
                format!("  {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The side line opened at a boundary on the main line: its banners, prompt and indentation, the
/// questions, `/abort`, and Ctrl-C back to the main line paused at the boundary. Named so that
/// `cargo test subline::open` selects them.
#[cfg(test)]
mod open {
    use std::fs;
    use std::process::ExitCode;

    use serde_json::json;
    use tempfile::TempDir;

    use super::super::tests::{input, project, reply};
    use super::super::{INTERRUPTED, PAUSED, main_line};
    use super::*;
    use crate::agent::mainline::MainLine;
    use crate::agent::tools::Tools;
    use crate::cli::ui::{ScriptedUi, Step};
    use crate::config::Config;
    use crate::llm::{ChatError, ChatResponse, FakeChatClient, Router, ToolCall};
    use crate::profile::store::Profile;
    use crate::workspace::cargo::FakeCargo;
    use crate::workspace::path::Root;

    const WITH_DBG: &str = "fn main() {\n    dbg!(1);\n}\n";
    const ENTER: &str = "── side line: dbg_macro ────────────────────────────────────";
    const LEAVE: &str = "── back to the main line ───────────────────────────────────";
    const HINT_SHOWN: &str = "  Ask anything about it, as many times as you like. Type /choose to choose how to go on, or /abort to withdraw the request.";
    const STOPPED: &str = "Stopped at a boundary: the request needs dbg_macro (macros), which your profile does not allow yet (the model could not avoid them after a rewrite). Nothing was written for it.";
    const SUB: &str = "molt [sub: dbg_macro]> ";

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

    /// A model that tries `dbg!` twice, which stops the first turn at a boundary, then answers
    /// with each of `replies` in turn.
    fn stops_at_a_boundary_then(replies: &[&str]) -> FakeChatClient {
        let client = FakeChatClient::new();
        client.push_chat(Ok(write_main("first")));
        client.push_chat(Ok(write_main("second")));
        for text in replies {
            client.push_chat(Ok(reply(text)));
        }
        client
    }

    /// A project with `src/main.rs` and an empty `profile.toml`.
    fn project_with_a_profile() -> TempDir {
        let dir = project();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(dir.path().join("profile.toml"), "").unwrap();
        dir
    }

    /// Runs the main line on `steps` in `dir`, with the profile read from its `profile.toml`.
    fn run_in(
        dir: &TempDir,
        client: &FakeChatClient,
        cargo: &FakeCargo,
        steps: impl IntoIterator<Item = Step>,
    ) -> (ExitCode, ScriptedUi) {
        let profile = Profile::load(&dir.path().join("profile.toml")).unwrap();
        let tools = Tools::new(Root::new(dir.path()).unwrap(), cargo);
        let mut agent = MainLine::new(client, Router::new(&Config::default()), tools, profile);
        let mut ui = ScriptedUi::new(steps);
        let code = main_line(&mut ui, &mut agent);
        (code, ui)
    }

    fn run(
        client: &FakeChatClient,
        steps: impl IntoIterator<Item = Step>,
    ) -> (ExitCode, ScriptedUi) {
        let dir = project_with_a_profile();
        run_in(&dir, client, &FakeCargo::new(), steps)
    }

    /// The content of every file in `dir` and in its `src`, the only directories it has.
    fn snapshot(dir: &std::path::Path) -> Vec<(String, Vec<u8>)> {
        let mut files = Vec::new();
        for sub in [dir.to_path_buf(), dir.join("src")] {
            let mut entries: Vec<_> = fs::read_dir(&sub)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .filter(|path| path.is_file())
                .collect();
            entries.sort();
            for path in entries {
                files.push((path.display().to_string(), fs::read(&path).unwrap()));
            }
        }
        files
    }

    #[test]
    fn a_boundary_opens_the_side_line_with_a_banner_its_prompt_and_the_explanation_indented() {
        let client = stops_at_a_boundary_then(&["dbg! prints a value.\n\nIt returns it, too."]);
        let (code, ui) = run(&client, [input("debug 1"), input("/abort"), input("/quit")]);

        assert_eq!(code, ExitCode::SUCCESS);
        assert_eq!(
            ui.shown()[2..6],
            [
                ENTER,
                "  dbg! prints a value.\n\n  It returns it, too.",
                HINT_SHOWN,
                LEAVE
            ]
        );
        assert_eq!(ui.asked(), ["molt> ", SUB, "molt> "]);
    }

    #[test]
    fn the_side_line_prompt_names_every_item_of_the_boundary() {
        let client = FakeChatClient::new();
        client.push_chat(Ok(ChatResponse {
            content: None,
            tool_calls: vec![ToolCall {
                id: "b".to_owned(),
                name: "request_boundary".to_owned(),
                arguments: json!({ "items": ["arc", "mutex"], "reason": "threads share it" })
                    .to_string(),
            }],
        }));
        client.push_chat(Ok(reply("Arc shares, Mutex locks.")));
        let (_, ui) = run(&client, [input("share it"), Step::Eof]);

        assert_eq!(ui.asked(), ["molt> ", "molt [sub: arc, mutex]> "]);
        assert_eq!(
            ui.shown()[2],
            "── side line: arc, mutex ───────────────────────────────────"
        );
    }

    #[test]
    fn questions_on_the_side_line_are_answered_indented_as_many_times_as_asked() {
        let client = stops_at_a_boundary_then(&["explanation", "answer 1", "answer 2", "answer 3"]);
        let (_, ui) = run(
            &client,
            [
                input("debug 1"),
                input("q1"),
                input("  "),
                input("q2"),
                input("q3"),
                input("/abort"),
                input("/quit"),
            ],
        );

        assert_eq!(
            ui.shown()[3..8],
            [
                "  explanation",
                HINT_SHOWN,
                "  answer 1",
                "  answer 2",
                "  answer 3"
            ]
        );
        assert_eq!(client.requests().len(), 6, "a blank line is not a question");
    }

    #[test]
    fn an_unknown_command_on_the_side_line_is_not_sent_to_the_model() {
        let client = stops_at_a_boundary_then(&["explanation"]);
        let (_, ui) = run(
            &client,
            [input("debug 1"), input("/quit"), input("/abort"), Step::Eof],
        );

        assert_eq!(ui.shown()[5], format!("  Unknown command /quit. {HINT}"));
        assert_eq!(client.requests().len(), 3);
    }

    #[test]
    fn a_failed_explanation_is_shown_and_the_side_line_still_takes_questions() {
        let client = stops_at_a_boundary_then(&[]);
        client.push_chat(Err(ChatError::FakeExhausted));
        client.push_chat(Ok(reply("answer")));
        let (_, ui) = run(
            &client,
            [input("debug 1"), input("what?"), input("/abort"), Step::Eof],
        );

        assert!(
            ui.shown()[3].starts_with("  Could not get the explanation: "),
            "{:?}",
            ui.shown()
        );
        assert_eq!(ui.shown()[5], "  answer");
    }

    #[test]
    fn abort_after_questions_on_the_side_line_changes_neither_the_files_nor_the_profile() {
        let client = stops_at_a_boundary_then(&["explanation", "answer", "unused"]);
        let dir = project_with_a_profile();
        let before = snapshot(dir.path());
        let cargo = FakeCargo::new();

        let (code, ui) = run_in(
            &dir,
            &client,
            &cargo,
            [
                input("debug 1"),
                input("why?"),
                input("/abort"),
                input("/quit"),
            ],
        );

        assert_eq!(code, ExitCode::SUCCESS);
        assert_eq!(ui.shown()[6..], [LEAVE, "Request withdrawn."]);
        assert_eq!(snapshot(dir.path()), before);
        assert!(cargo.calls().is_empty());
    }

    #[test]
    fn ctrl_c_on_the_side_line_pauses_the_main_line_at_the_boundary_and_side_resumes_the_same_side_line()
     {
        let client = stops_at_a_boundary_then(&["explanation", "answer 1", "answer 2"]);
        let (code, ui) = run(
            &client,
            [
                input("debug 1"),
                input("q1"),
                Step::Interrupt,
                input("go on"),
                input("/side"),
                input("q2"),
                input("/abort"),
                input("/quit"),
            ],
        );

        assert_eq!(code, ExitCode::SUCCESS);
        assert_eq!(
            ui.shown(),
            [
                STOPPED,
                "Tool calls in this turn: write_file (2)",
                ENTER,
                "  explanation",
                HINT_SHOWN,
                "  answer 1",
                LEAVE,
                PAUSED,
                PAUSED,
                ENTER,
                HINT_SHOWN,
                "  answer 2",
                LEAVE,
                "Request withdrawn.",
            ]
        );
        assert_eq!(
            ui.asked(),
            ["molt> ", SUB, SUB, "molt> ", "molt> ", SUB, SUB, "molt> "]
        );
        let requests = client.requests();
        assert_eq!(
            requests.len(),
            5,
            "no second explanation, and nothing sent while paused"
        );
        let last = format!("{:?}", requests[4].messages);
        assert!(
            last.contains("q1") && last.contains("answer 1"),
            "the same side line: {last}"
        );
    }

    #[test]
    fn ctrl_c_on_the_side_line_changes_neither_the_files_nor_the_profile() {
        let client = stops_at_a_boundary_then(&["explanation"]);
        let dir = project_with_a_profile();
        let before = snapshot(dir.path());
        let cargo = FakeCargo::new();

        run_in(
            &dir,
            &client,
            &cargo,
            [input("debug 1"), Step::Interrupt, Step::Eof],
        );

        assert_eq!(snapshot(dir.path()), before);
        assert!(cargo.calls().is_empty());
    }

    #[test]
    fn abort_while_paused_withdraws_the_request_and_the_next_input_goes_to_the_main_line() {
        let client = stops_at_a_boundary_then(&["explanation", "hello again"]);
        let (code, ui) = run(
            &client,
            [
                input("debug 1"),
                Step::Interrupt,
                input("/abort"),
                input("next"),
                input("/quit"),
            ],
        );

        assert_eq!(code, ExitCode::SUCCESS);
        assert_eq!(
            ui.shown()[5..],
            [LEAVE, PAUSED, "Request withdrawn.", "hello again"]
        );
    }

    #[test]
    fn ctrl_c_while_paused_ends_the_main_line_with_a_non_zero_code() {
        let client = stops_at_a_boundary_then(&["explanation"]);
        let (code, _) = run(
            &client,
            [input("debug 1"), Step::Interrupt, Step::Interrupt],
        );
        assert_eq!(code, ExitCode::from(INTERRUPTED));
    }

    #[test]
    fn ctrl_d_while_paused_ends_the_main_line_with_success() {
        let client = stops_at_a_boundary_then(&["explanation"]);
        let (code, _) = run(&client, [input("debug 1"), Step::Interrupt, Step::Eof]);
        assert_eq!(code, ExitCode::SUCCESS);
    }

    #[test]
    fn a_session_log_that_cannot_be_written_on_the_side_line_is_shown_once_right_away() {
        let dir = project_with_a_profile();
        let logs = tempfile::tempdir().unwrap();
        let path = logs.path().join("read-only.jsonl");
        fs::write(&path, "").unwrap();
        let client = stops_at_a_boundary_then(&["explanation", "answer"]);
        let cargo = FakeCargo::new();
        let profile = Profile::load(&dir.path().join("profile.toml")).unwrap();
        let tools = Tools::new(Root::new(dir.path()).unwrap(), &cargo);
        let mut agent = MainLine::new(&client, Router::new(&Config::default()), tools, profile);
        let crate::agent::mainline::TurnEnd::Boundary(boundary) = agent
            .turn(
                "debug 1",
                &mut crate::cli::ui::UiApprover::new(&mut ScriptedUi::new([])),
            )
            .end
        else {
            panic!("the turn did not stop at a boundary");
        };
        let mut side = agent.side_line(boundary);
        let log = SessionLog::unwritable_requests(&path);
        let mut ui = ScriptedUi::new([input("why?"), Step::Eof]);

        assert_eq!(super::run(&mut ui, &mut side, &log), Left::Eof);

        let warning = format!("Could not write the session log {}: ", path.display());
        let shown = ui.shown();
        let warnings: Vec<usize> = shown
            .iter()
            .enumerate()
            .filter(|(_, text)| text.starts_with(&warning))
            .map(|(at, _)| at)
            .collect();
        assert_eq!(warnings.len(), 1, "{shown:?}");
        assert_eq!(shown[warnings[0] - 1], "  explanation", "{shown:?}");
    }

    #[test]
    fn indent_puts_2_spaces_before_each_line_but_blank_ones() {
        assert_eq!(indent("a\n\n  b\n"), "  a\n\n    b");
    }

    /// Ctrl-C while the side line waits for the model (UC3's exception flow): the request is
    /// dropped and the main line is paused at the boundary, with nothing changed. The client
    /// returns [`ChatError::Interrupted`] as the real one does on Ctrl-C. Named so that
    /// `cargo test while_waiting` selects them.
    mod while_waiting {
        use super::*;

        #[test]
        fn interrupting_the_explanation_pauses_the_main_line_and_side_asks_for_it_again() {
            let client = stops_at_a_boundary_then(&[]);
            client.push_chat(Err(ChatError::Interrupted));
            client.push_chat(Ok(reply("explanation")));
            let (code, ui) = run(
                &client,
                [
                    input("debug 1"),
                    input("/side"),
                    input("/abort"),
                    input("/quit"),
                ],
            );

            assert_eq!(code, ExitCode::SUCCESS);
            assert_eq!(
                ui.shown()[2..],
                [
                    ENTER,
                    LEAVE,
                    PAUSED,
                    ENTER,
                    "  explanation",
                    HINT_SHOWN,
                    LEAVE,
                    "Request withdrawn."
                ]
            );
            assert_eq!(ui.asked(), ["molt> ", "molt> ", SUB, "molt> "]);
            let requests = client.requests();
            assert_eq!(requests.len(), 4, "the explanation is asked for once more");
            assert_eq!(requests[3], requests[2]);
        }

        #[test]
        fn an_interrupted_question_is_left_out_of_the_side_line_and_the_main_line_is_paused() {
            let client = stops_at_a_boundary_then(&["explanation"]);
            client.push_chat(Err(ChatError::Interrupted));
            client.push_chat(Ok(reply("answer 2")));
            let (code, ui) = run(
                &client,
                [
                    input("debug 1"),
                    input("q1"),
                    input("/side"),
                    input("q2"),
                    input("/abort"),
                    input("/quit"),
                ],
            );

            assert_eq!(code, ExitCode::SUCCESS);
            assert_eq!(
                ui.shown()[5..],
                [
                    LEAVE,
                    PAUSED,
                    ENTER,
                    HINT_SHOWN,
                    "  answer 2",
                    LEAVE,
                    "Request withdrawn."
                ]
            );
            let requests = client.requests();
            assert_eq!(requests.len(), 5);
            let next = format!("{:?}", requests[4].messages);
            assert!(next.contains("q2"), "{next}");
            assert!(!next.contains("q1"), "{next}");
        }

        #[test]
        fn interrupting_the_explanation_changes_neither_the_files_nor_the_profile() {
            let client = stops_at_a_boundary_then(&[]);
            client.push_chat(Err(ChatError::Interrupted));
            let dir = project_with_a_profile();
            let before = snapshot(dir.path());
            let cargo = FakeCargo::new();

            let (code, _) = run_in(&dir, &client, &cargo, [input("debug 1"), Step::Eof]);

            assert_eq!(code, ExitCode::SUCCESS);
            assert_eq!(snapshot(dir.path()), before);
            assert!(cargo.calls().is_empty());
        }

        #[test]
        fn interrupting_a_question_changes_neither_the_files_nor_the_profile() {
            let client = stops_at_a_boundary_then(&["explanation"]);
            client.push_chat(Err(ChatError::Interrupted));
            let dir = project_with_a_profile();
            let before = snapshot(dir.path());
            let cargo = FakeCargo::new();

            let (code, _) = run_in(
                &dir,
                &client,
                &cargo,
                [input("debug 1"), input("q1"), Step::Eof],
            );

            assert_eq!(code, ExitCode::SUCCESS);
            assert_eq!(snapshot(dir.path()), before);
            assert!(cargo.calls().is_empty());
        }
    }
}
