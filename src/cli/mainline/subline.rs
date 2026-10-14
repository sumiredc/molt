//! The side line on the terminal (PRD UC3, §8.6): a ruled banner on entering and leaving, its own
//! prompt `molt [sub: arc, mutex]> `, and its output indented by 2 spaces.

mod resolve;
mod user_wrote;

use super::show_error_of;
use crate::agent::mainline::{Boundary, MainLine};
use crate::agent::subline::{Choice, Resolution, SubLine};
use crate::llm::ChatError;
use crate::profile::status::ItemKey;
use crate::ui::markdown::HoldFirstCode;
use crate::ui::{Input, Tone, Ui};

/// The width of the banners, in characters.
const BANNER_WIDTH: usize = 60;

/// What the user is shown while the explanation comes, and while an answer comes (PRD §8.1).
const EXPLAINING: &str = "Explaining…";
/// What the user is shown while `/done` checks and reviews their lines (PRD §8.1).
const CHECKING: &str = "Checking your lines…";
pub(super) const THINKING: &str = "Thinking…";

/// What the user can do on the side line (UC3).
const HINT: &str = "Ask anything about it, as many times as you like. Type /choose to choose how to go on, \
     or /abort to withdraw the request.";

/// How the user left the side line.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Left {
    /// `/choose`, and the user chose ① or ②.
    Chose(Choice),
    /// ③, and `/done` passed: the outcome to hand back to the main line.
    Wrote(Resolution),
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

/// Runs the side line of `agent`, which stopped at its boundary, until the user leaves it. The
/// explanation is asked for the first time only; when the side line is opened again, the
/// questions go on where they were, and so does ③ when the user was writing.
pub(super) fn run(ui: &mut dyn Ui, side: &mut SubLine, agent: &mut MainLine) -> Left {
    ui.show(&banner(&format!(
        "side line: {}",
        names(&side.boundary().items)
    )));
    if !side.explained() {
        // Before the explanation, which takes a while and refers to them (PRD §8.6).
        ui.show_lines(&step_lines(side.boundary()));
        match explain(ui, side, agent) {
            Ok(()) => {}
            Err(ChatError::Interrupted) => {
                show_error_of(ui, agent.log());
                return leave(ui, Left::Interrupted);
            }
            Err(error) => ui.show(&indent(&format!("Could not get the explanation: {error}"))),
        }
        show_error_of(ui, agent.log());
    }
    if side.is_recurrence() {
        ui.show(&indent(&resolve::recurred(&side.boundary().items)));
    }
    ui.show(&indent(hint(side)));
    let prompt = prompt(&side.boundary().items);
    let left = loop {
        match ui.read_line(&prompt) {
            Ok(Input::Line(line)) => {
                let line = line.trim();
                if line == "/abort" {
                    break Left::Aborted;
                } else if line == "/choose" {
                    match resolve::choose(ui, side) {
                        Ok(Some(Choice::UserWrote)) => {
                            if let Some(left) = user_wrote::guide(ui, side, agent) {
                                break left;
                            }
                        }
                        Ok(Some(choice)) => break Left::Chose(choice),
                        Ok(None) => ui.show(&indent(hint(side))),
                        Err(error) => {
                            eprintln!("{error}");
                            return Left::Failed;
                        }
                    }
                } else if line == "/done" {
                    if let Some(left) = user_wrote::done(ui, side, agent) {
                        break left;
                    }
                } else if line.is_empty() {
                } else if line.starts_with('/') {
                    ui.show(&indent(&format!("Unknown command {line}. {}", hint(side))));
                } else {
                    match ask(ui, side, agent, line) {
                        Ok(()) => {}
                        Err(ChatError::Interrupted) => {
                            show_error_of(ui, agent.log());
                            break Left::Interrupted;
                        }
                        Err(error) => {
                            ui.show(&indent(&format!("Could not get an answer: {error}")))
                        }
                    }
                    show_error_of(ui, agent.log());
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

/// Gets the explanation and shows it (PRD §8.1). When the [`Ui`] streams, the items come first,
/// the prose as it arrives with the first code block held back, and that block, the comparison,
/// once the reply has come; otherwise the whole explanation once it has come.
fn explain(ui: &mut dyn Ui, side: &mut SubLine, agent: &MainLine) -> Result<(), ChatError> {
    let streams = ui.streams();
    if streams {
        ui.show_markdown(&side.explanation_head(), "  ");
    }
    ui.status(Some(EXPLAINING));
    let mut hold = HoldFirstCode::default();
    let explained = side.explain(agent.log(), &mut |line| {
        if streams && hold.passes(line) {
            ui.stream_line(line, "  ");
        }
    });
    if streams {
        ui.end_stream();
    }
    ui.status(None);
    let explanation = explained?;
    match (streams, &explanation.tail) {
        (true, Some(tail)) => ui.show_markdown(tail, "  "),
        (true, None) => {}
        (false, _) => ui.show_markdown(&explanation.to_string(), "  "),
    }
    Ok(())
}

/// Asks `question` and shows the answer: as it arrives when the [`Ui`] streams, otherwise once it
/// has come (PRD §8.1).
fn ask(
    ui: &mut dyn Ui,
    side: &mut SubLine,
    agent: &MainLine,
    question: &str,
) -> Result<(), ChatError> {
    let streams = ui.streams();
    ui.status(Some(THINKING));
    let answered = side.ask(question, agent.log(), &mut |line| {
        if streams {
            ui.stream_line(line, "  ");
        }
    });
    if streams {
        ui.end_stream();
    }
    ui.status(None);
    let answer = answered?;
    if !streams {
        ui.show_markdown(&answer, "  ");
    }
    Ok(())
}

/// Shows the banner of leaving the side line, and returns `left`.
fn leave(ui: &mut dyn Ui, left: Left) -> Left {
    ui.show(&banner("back to the main line"));
    left
}

/// What the user can do on `side` now: [`HINT`], or while they write in ③, what to send when done.
fn hint(side: &SubLine) -> &'static str {
    if side.is_writing() {
        user_wrote::WRITING
    } else {
        HINT
    }
}

/// The lines shown around each changed line of a step (PRD §8.6).
const AROUND: usize = 2;

/// The step of `boundary` as the user is shown it on entering the side line (PRD §8.6), indented:
/// its file, then each changed line with [`AROUND`] lines before and after it, those dim, and `⋮`
/// between parts that are apart, so that the numbers run on except at a `⋮`.
pub(super) fn step_lines(boundary: &Boundary) -> Vec<(Tone, String)> {
    let step = &boundary.step;
    let text: Vec<&str> = step.after.lines().collect();
    let changed: Vec<usize> = step.lines.iter().map(|(line, _)| *line).collect();
    // 1-based numbers of the lines to show, in order.
    let mut shown: Vec<usize> = Vec::new();
    for &line in &changed {
        let first = line.saturating_sub(AROUND).max(1);
        let last = (line + AROUND).min(text.len());
        for number in first..=last {
            if shown.last().is_none_or(|&last| number > last) {
                shown.push(number);
            }
        }
    }
    let width = shown.last().map_or(1, |line| line.to_string().len());
    let mut lines = vec![(
        Tone::Normal,
        format!("  The step molt wrote to {}:", step.path),
    )];
    let mut previous = None;
    for number in shown {
        if previous.is_some_and(|previous| number > previous + 1) {
            lines.push((Tone::Dim, format!("  {:>width$} ⋮", "")));
        }
        previous = Some(number);
        let tone = if changed.contains(&number) {
            Tone::Normal
        } else {
            Tone::Dim
        };
        let code = text[number - 1];
        let line = format!("  {number:>width$} | {code}");
        lines.push((tone, line.trim_end().to_owned()));
    }
    lines
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
    use crate::config::Config;
    use crate::llm::{ChatError, ChatResponse, FakeChatClient, Router, ToolCall};
    use crate::profile::store::Profile;
    use crate::session::SessionLog;
    use crate::ui::{ScriptedUi, Step};
    use crate::workspace::cargo::FakeCargo;
    use crate::workspace::path::Root;

    const WITH_DBG: &str = "fn main() {\n    dbg!(1);\n}\n";
    const ENTER: &str = "── side line: dbg_macro ────────────────────────────────────";
    const LEAVE: &str = "── back to the main line ───────────────────────────────────";
    const HINT_SHOWN: &str = "  Ask anything about it, as many times as you like. Type /choose to choose how to go on, or /abort to withdraw the request.";
    const STOPPED: &str = "Paused after a step: molt wrote 3 lines to src/main.rs with dbg_macro (macros), which you have not written yet.";
    /// The step of the boundary, as shown before the explanation.
    const STEP: &str =
        "  The step molt wrote to src/main.rs:\n  1 | fn main() {\n  2 |     dbg!(1);\n  3 | }";
    const SUB: &str = "molt [sub: dbg_macro]> ";
    const TAKEN: &str = "molt took the step back: src/main.rs is as it was before it.";
    /// The explanation shown for the model's reply `explanation`, which has no code, so no
    /// comparison.
    const EXPLAINED: &str = "  `dbg_macro` (macros)\n\n  explanation";

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

    /// A model that writes `dbg!`, a step that stops the first turn at a boundary, then answers
    /// with each of `replies` in turn.
    fn stops_at_a_boundary_then(replies: &[&str]) -> FakeChatClient {
        let client = FakeChatClient::new();
        client.push_chat(Ok(write_main("first")));
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
    fn where_text_streams_the_explanation_comes_as_it_arrives_and_its_code_after_the_prose() {
        let client = stops_at_a_boundary_then(&[
            "dbg! prints a value.\n```rust\nfn main() {}\n```\nIt returns it, too.",
            "To stderr.\n\n```rust\neprintln!();\n```",
        ]);
        let dir = project_with_a_profile();
        let profile = Profile::load(&dir.path().join("profile.toml")).unwrap();
        let cargo = FakeCargo::new();
        let tools = Tools::new(Root::new(dir.path()).unwrap(), &cargo);
        let mut agent = MainLine::new(&client, Router::new(&Config::default()), tools, profile);
        let mut ui = ScriptedUi::new([
            input("debug 1"),
            input("where does it print?"),
            input("/abort"),
            input("/quit"),
        ])
        .streaming();

        main_line(&mut ui, &mut agent);

        let shown = ui.shown();
        let at = shown.iter().position(|text| text == ENTER).unwrap();
        assert_eq!(
            shown[at + 1..at + 11],
            [
                STEP,
                "  `dbg_macro` (macros)",
                "  dbg! prints a value.",
                "  It returns it, too.",
                "  Without `dbg_macro`, within your profile:\n  ```rust\n  fn main() {}\n  ```",
                HINT_SHOWN,
                // An answer streams whole, its code included.
                "  To stderr.",
                "",
                "  ```rust",
                "  eprintln!();",
            ],
            "{shown:?}"
        );
        let statuses: Vec<Option<&str>> = ui.statuses().iter().map(Option::as_deref).collect();
        assert!(statuses.contains(&Some("Explaining…")), "{statuses:?}");
        assert!(statuses.contains(&Some("Thinking…")), "{statuses:?}");
        assert_eq!(statuses.last(), Some(&None), "{statuses:?}");
    }

    #[test]
    fn a_boundary_opens_the_side_line_with_a_banner_its_prompt_and_the_explanation_indented() {
        let client = stops_at_a_boundary_then(&[
            "dbg! prints a value.\n\nIt returns it, too.\n\n```rust\nfn main() {\n    eprintln!(\"{}\", 1);\n}\n```",
        ]);
        let (code, ui) = run(&client, [input("debug 1"), input("/abort"), input("/quit")]);

        assert_eq!(code, ExitCode::SUCCESS);
        assert_eq!(
            ui.shown()[2..7],
            [
                ENTER,
                "  The step molt wrote to src/main.rs:\n  1 | fn main() {\n  2 |     dbg!(1);\n  3 | }",
                "  `dbg_macro` (macros)\n\n  dbg! prints a value.\n\n  It returns it, too.\n\n  Without `dbg_macro`, within your profile:\n  ```rust\n  fn main() {\n      eprintln!(\"{}\", 1);\n  }\n  ```",
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
                name: "write_file".to_owned(),
                arguments: json!({
                    "path": "src/main.rs",
                    "content": "use std::sync::{Arc, Mutex};\n\nfn main() {\n    let _a = Arc::new(Mutex::new(0));\n}\n",
                })
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
    fn ending_molt_at_a_boundary_takes_the_step_back_first() {
        for (steps, code) in [
            (vec![input("debug 1"), Step::Eof], ExitCode::SUCCESS),
            (
                vec![input("debug 1"), Step::Interrupt, Step::Eof],
                ExitCode::SUCCESS,
            ),
            (
                vec![input("debug 1"), Step::Interrupt, Step::Interrupt],
                ExitCode::from(INTERRUPTED),
            ),
        ] {
            let client = stops_at_a_boundary_then(&["explanation"]);
            let dir = project_with_a_profile();
            let (ended, ui) = run_in(&dir, &client, &FakeCargo::new(), steps);

            assert_eq!(ended, code);
            assert!(ui.shown().contains(&TAKEN.to_owned()), "{:?}", ui.shown());
            assert_eq!(
                fs::read_to_string(dir.path().join("src/main.rs")).unwrap(),
                "fn main() {}\n"
            );
            assert_eq!(
                fs::read_to_string(dir.path().join("profile.toml")).unwrap(),
                ""
            );
        }
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
            ui.shown()[3..9],
            [
                STEP,
                EXPLAINED,
                HINT_SHOWN,
                "  answer 1",
                "  answer 2",
                "  answer 3"
            ]
        );
        assert_eq!(client.requests().len(), 5, "a blank line is not a question");
    }

    #[test]
    fn an_unknown_command_on_the_side_line_is_not_sent_to_the_model() {
        let client = stops_at_a_boundary_then(&["explanation"]);
        let (_, ui) = run(
            &client,
            [input("debug 1"), input("/quit"), input("/abort"), Step::Eof],
        );

        assert_eq!(ui.shown()[6], format!("  Unknown command /quit. {HINT}"));
        assert_eq!(client.requests().len(), 2);
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
            ui.shown()[4].starts_with("  Could not get the explanation: "),
            "{:?}",
            ui.shown()
        );
        assert_eq!(ui.shown()[6], "  answer");
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
        assert_eq!(ui.shown()[7..], [LEAVE, "Request withdrawn.", TAKEN]);
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
                "● write_file src/main.rs",
                STOPPED,
                ENTER,
                STEP,
                EXPLAINED,
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
                TAKEN,
            ]
        );
        assert_eq!(
            ui.asked(),
            ["molt> ", SUB, SUB, "molt> ", "molt> ", SUB, SUB, "molt> "]
        );
        let requests = client.requests();
        assert_eq!(
            requests.len(),
            4,
            "no second explanation, and nothing sent while paused"
        );
        let last = format!("{:?}", requests[3].messages);
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
            ui.shown()[6..],
            [LEAVE, PAUSED, "Request withdrawn.", TAKEN, "hello again"]
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
                &mut crate::cli::mainline::approver::UiApprover::new(&mut ScriptedUi::new([])),
            )
            .end
        else {
            panic!("the turn did not stop at a boundary");
        };
        let mut side = agent.side_line(boundary);
        // The log the side line writes to, from here on: the turn before did not fail to write.
        let mut agent = agent.with_log(SessionLog::unwritable_requests(&path));
        let mut ui = ScriptedUi::new([input("why?"), Step::Eof]);

        assert_eq!(super::run(&mut ui, &mut side, &mut agent), Left::Eof);

        let warning = format!("Could not write the session log {}: ", path.display());
        let shown = ui.shown();
        let warnings: Vec<usize> = shown
            .iter()
            .enumerate()
            .filter(|(_, text)| text.starts_with(&warning))
            .map(|(at, _)| at)
            .collect();
        assert_eq!(warnings.len(), 1, "{shown:?}");
        assert_eq!(shown[warnings[0] - 1], EXPLAINED, "{shown:?}");
    }

    fn step_over(before: &str, after: &str) -> Boundary {
        Boundary {
            items: Vec::new(),
            step: crate::agent::mainline::Step::new(
                "src/main.rs".to_owned(),
                Some(before.to_owned()),
                after.to_owned(),
            ),
        }
    }

    #[test]
    fn a_step_shows_2_dim_lines_around_each_changed_one_and_numbers_run_on_but_at_a_gap() {
        let before: String = (1..=12).map(|n| format!("l{n}\n")).collect();
        let after = before.replace("l3\n", "L3\n").replace("l11\n", "L11\n");

        let lines = step_lines(&step_over(&before, &after));

        use Tone::{Dim, Normal};
        let expected: Vec<(Tone, String)> = [
            (Normal, "  The step molt wrote to src/main.rs:"),
            (Dim, "   1 | l1"),
            (Dim, "   2 | l2"),
            (Normal, "   3 | L3"),
            (Dim, "   4 | l4"),
            (Dim, "   5 | l5"),
            (Dim, "     ⋮"),
            (Dim, "   9 | l9"),
            (Dim, "  10 | l10"),
            (Normal, "  11 | L11"),
            (Dim, "  12 | l12"),
        ]
        .into_iter()
        .map(|(tone, line)| (tone, line.to_owned()))
        .collect();
        assert_eq!(lines, expected);
    }

    #[test]
    fn parts_of_a_step_that_touch_are_shown_without_a_gap() {
        let before = "a\nb\nc\nd\ne\nf\ng\nh\n";
        let after = "A\nb\nc\nd\ne\nF\ng\nh\n";

        let lines = step_lines(&step_over(before, after));

        let numbers: Vec<&str> = lines[1..].iter().map(|(_, line)| line.trim()).collect();
        assert_eq!(
            numbers,
            [
                "1 | A", "2 | b", "3 | c", "4 | d", "5 | e", "6 | F", "7 | g", "8 | h"
            ]
        );
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
                    STEP,
                    LEAVE,
                    PAUSED,
                    ENTER,
                    STEP,
                    EXPLAINED,
                    HINT_SHOWN,
                    LEAVE,
                    "Request withdrawn.",
                    TAKEN
                ]
            );
            assert_eq!(ui.asked(), ["molt> ", "molt> ", SUB, "molt> "]);
            let requests = client.requests();
            assert_eq!(requests.len(), 3, "the explanation is asked for once more");
            assert_eq!(requests[2], requests[1]);
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
                ui.shown()[6..],
                [
                    LEAVE,
                    PAUSED,
                    ENTER,
                    HINT_SHOWN,
                    "  answer 2",
                    LEAVE,
                    "Request withdrawn.",
                    TAKEN
                ]
            );
            let requests = client.requests();
            assert_eq!(requests.len(), 4);
            let next = format!("{:?}", requests[3].messages);
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

        #[test]
        fn interrupting_the_policy_of_rewrite_changes_nothing_and_pauses_the_main_line() {
            let client = stops_at_a_boundary_then(&["explanation"]);
            client.push_chat(Err(ChatError::Interrupted));
            let dir = project_with_a_profile();
            let before = snapshot(dir.path());
            let cargo = FakeCargo::new();

            let (code, ui) = run_in(
                &dir,
                &client,
                &cargo,
                [
                    input("debug 1"),
                    input("/choose"),
                    Step::Select(2),
                    input("/side"),
                    input("/abort"),
                    input("/quit"),
                ],
            );

            assert_eq!(code, ExitCode::SUCCESS);
            assert_eq!(
                ui.shown()[6..],
                [
                    LEAVE,
                    PAUSED,
                    ENTER,
                    HINT_SHOWN,
                    LEAVE,
                    "Request withdrawn.",
                    TAKEN
                ]
            );
            assert_eq!(client.requests().len(), 3, "nothing sent after the policy");
            assert_eq!(snapshot(dir.path()), before);
            assert!(cargo.calls().is_empty());
        }
    }
}
