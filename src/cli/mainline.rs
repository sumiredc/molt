//! `molt` without a subcommand: checks UC2's preconditions, then opens the main line in the cargo
//! project at cwd (PRD §8.6).

use std::path::Path;
use std::process::ExitCode;

use super::profile;
use super::ui::{Input, ScriptedUi, TerminalUi, Ui, UiApprover};
use crate::agent::mainline::{MainLine, TOOL_CALL_BUDGET, Turn, TurnEnd};
use crate::agent::tools::Tools;
use crate::config::{self, Config};
use crate::llm::{OpenAiClient, Router};
use crate::workspace::cargo::Cargo;
use crate::workspace::path::Root;

/// The main-line prompt (PRD §8.6).
const PROMPT: &str = "molt> ";

/// The exit code after Ctrl-C at the prompt (128 + SIGINT, as a shell reports it).
const INTERRUPTED: u8 = 130;

pub(super) fn run(profile_flag: Option<&Path>, ui_script: Option<&Path>) -> ExitCode {
    let opened = preconditions(profile_flag).and_then(|root| {
        let (config, client) = connect()?;
        let ui = open_ui(ui_script)?;
        Ok((root, config, client, ui))
    });
    let (root, config, client, mut ui) = match opened {
        Ok(opened) => opened,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };
    let cargo = Cargo::new(root.clone());
    let tools = Tools::new(root, &cargo);
    let mut agent = MainLine::new(&client, Router::new(&config), tools);
    main_line(ui.as_mut(), &mut agent)
}

/// UC2's preconditions: a profile exists, and the current directory is a cargo project. Returns
/// that directory as the root the tools are confined to.
fn preconditions(profile_flag: Option<&Path>) -> Result<Root, String> {
    let path = profile::profile_path(profile_flag)?;
    profile::load(&path)?;
    let cwd = std::env::current_dir()
        .map_err(|error| format!("cannot read the current directory: {error}"))?;
    if !cwd.join("Cargo.toml").is_file() {
        return Err(format!(
            "not a cargo project: no Cargo.toml in {}",
            cwd.display()
        ));
    }
    Root::new(&cwd).map_err(|error| error.to_string())
}

/// Reads `config.toml` and creates the client for its endpoint. The main line does not open
/// when the client cannot be created, e.g. the API key's environment variable is unset.
fn connect() -> Result<(Config, OpenAiClient), String> {
    let home = config::molt_home(|name| std::env::var_os(name)).map_err(|e| e.to_string())?;
    let config = Config::load(&config::config_path(&home)).map_err(|e| e.to_string())?;
    let client = OpenAiClient::new(&config.api, |name| std::env::var(name).ok())
        .map_err(|e| e.to_string())?;
    Ok((config, client))
}

/// The scripted UI when `--ui-script` is given, otherwise the terminal.
fn open_ui(ui_script: Option<&Path>) -> Result<Box<dyn Ui>, String> {
    match ui_script {
        Some(path) => ScriptedUi::load(path)
            .map(|ui| Box::new(ui.echo_to_stdout()) as Box<dyn Ui>)
            .map_err(|error| error.to_string()),
        None => TerminalUi::new()
            .map(|ui| Box::new(ui) as Box<dyn Ui>)
            .map_err(|error| error.to_string()),
    }
}

/// Reads requests until `/quit` or Ctrl-D (exit 0) or Ctrl-C (exit non-zero), and runs a turn
/// of `agent` for each. A blank line is not sent to the model.
fn main_line(ui: &mut dyn Ui, agent: &mut MainLine) -> ExitCode {
    loop {
        match ui.read_line(PROMPT) {
            Ok(Input::Line(line)) if line.trim() == "/quit" => return ExitCode::SUCCESS,
            Ok(Input::Line(line)) if line.trim().is_empty() => {}
            Ok(Input::Line(line)) => {
                let turn = agent.turn(&line, &mut UiApprover::new(ui));
                show_turn(ui, &turn);
            }
            Ok(Input::Eof) => return ExitCode::SUCCESS,
            Ok(Input::Interrupted) => return ExitCode::from(INTERRUPTED),
            Err(error) => {
                eprintln!("{error}");
                return ExitCode::FAILURE;
            }
        }
    }
}

/// Shows what the model said last. When the turn stopped instead of finishing, also shows why
/// and the tool calls made so far.
fn show_turn(ui: &mut dyn Ui, turn: &Turn) {
    if let Some(reply) = &turn.reply {
        ui.show(reply);
    }
    let stopped = match &turn.end {
        TurnEnd::Finished => return,
        TurnEnd::OverBudget => {
            format!("Stopped: this turn reached the limit of {TOOL_CALL_BUDGET} tool calls.")
        }
        TurnEnd::InvalidArguments(error) => format!(
            "Stopped: the model sent tool arguments that are not valid JSON again after being asked to resend them: {error}"
        ),
        TurnEnd::ApiFailed(error) => format!("Stopped: {error}"),
    };
    ui.show(&stopped);
    if !turn.tool_calls.is_empty() {
        ui.show(&format!(
            "Tool calls in this turn: {}",
            count_by_name(&turn.tool_calls)
        ));
    }
}

/// `read_file (2), run_cargo (1)`, in the order each name first appears.
fn count_by_name(names: &[String]) -> String {
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for name in names {
        match counts.iter_mut().find(|(seen, _)| seen == name) {
            Some((_, count)) => *count += 1,
            None => counts.push((name, 1)),
        }
    }
    counts
        .iter()
        .map(|(name, count)| format!("{name} ({count})"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::cli::ui::Step;
    use crate::llm::{ChatResponse, FakeChatClient};
    use crate::workspace::cargo::FakeCargo;

    pub(super) fn project() -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\n",
        )
        .unwrap();
        dir
    }

    /// Runs the main line on `steps` against `client`, in a cargo project with no cargo.
    pub(super) fn run_script(
        client: &FakeChatClient,
        steps: impl IntoIterator<Item = Step>,
    ) -> (ExitCode, ScriptedUi) {
        let dir = project();
        let cargo = FakeCargo::new();
        let tools = Tools::new(Root::new(dir.path()).unwrap(), &cargo);
        let mut agent = MainLine::new(client, Router::new(&Config::default()), tools);
        let mut ui = ScriptedUi::new(steps);
        let code = main_line(&mut ui, &mut agent);
        (code, ui)
    }

    pub(super) fn input(line: &str) -> Step {
        Step::Input(line.to_owned())
    }

    pub(super) fn reply(text: &str) -> ChatResponse {
        ChatResponse {
            content: Some(text.to_owned()),
            tool_calls: Vec::new(),
        }
    }

    #[test]
    fn quit_ends_the_main_line_with_success() {
        let (code, _) = run_script(&FakeChatClient::new(), [input("/quit")]);
        assert_eq!(code, ExitCode::SUCCESS);
    }

    #[test]
    fn quit_with_surrounding_spaces_still_ends_the_main_line() {
        let (code, ui) = run_script(&FakeChatClient::new(), [input("  /quit "), input("/quit")]);
        assert_eq!(code, ExitCode::SUCCESS);
        assert_eq!(
            ui.asked().len(),
            1,
            "the main line ended at the first /quit"
        );
    }

    #[test]
    fn eof_ends_the_main_line_with_success() {
        let (code, _) = run_script(&FakeChatClient::new(), [Step::Eof]);
        assert_eq!(code, ExitCode::SUCCESS);
    }

    #[test]
    fn interrupt_ends_the_main_line_with_a_non_zero_code() {
        let (code, _) = run_script(&FakeChatClient::new(), [Step::Interrupt]);
        assert_eq!(code, ExitCode::from(INTERRUPTED));
    }

    #[test]
    fn a_request_goes_to_the_model_and_its_reply_is_shown_before_the_prompt_comes_back() {
        let client = FakeChatClient::new();
        client.push_chat(Ok(reply("Done.")));
        let (code, ui) = run_script(&client, [input("write a parser"), input("/quit")]);
        assert_eq!(code, ExitCode::SUCCESS);
        assert_eq!(ui.asked(), ["molt> "; 2]);
        assert_eq!(ui.shown(), ["Done."]);
        assert_eq!(client.requests().len(), 1);
    }

    #[test]
    fn a_blank_line_is_not_sent_to_the_model() {
        let client = FakeChatClient::new();
        let (code, ui) = run_script(&client, [input(""), input("  "), input("/quit")]);
        assert_eq!(code, ExitCode::SUCCESS);
        assert_eq!(ui.asked(), ["molt> "; 3]);
        assert!(client.requests().is_empty());
    }

    #[test]
    fn a_ui_error_ends_the_main_line_with_failure() {
        let (code, _) = run_script(&FakeChatClient::new(), []);
        assert_eq!(code, ExitCode::FAILURE);
    }

    #[test]
    fn count_by_name_counts_each_tool_in_the_order_it_first_appears() {
        let names = ["read_file", "run_cargo", "read_file"].map(str::to_owned);
        assert_eq!(count_by_name(&names), "read_file (2), run_cargo (1)");
    }
}

/// How a turn shows on the main line. Named so that `cargo test mainline::turn` selects them.
#[cfg(test)]
mod turn {
    use std::fs;

    use super::tests::{input, project, reply};
    use super::*;
    use crate::llm::{ChatError, ChatResponse, FakeChatClient, ToolCall};
    use crate::workspace::cargo::FakeCargo;

    fn read_main(id: &str) -> ChatResponse {
        ChatResponse {
            content: None,
            tool_calls: vec![ToolCall {
                id: id.to_owned(),
                name: "read_file".to_owned(),
                arguments: r#"{"path": "Cargo.toml"}"#.to_owned(),
            }],
        }
    }

    #[test]
    fn an_api_error_shows_its_cause_leaves_the_files_unchanged_and_the_prompt_comes_back() {
        let dir = project();
        let before = fs::read(dir.path().join("Cargo.toml")).unwrap();
        let client = FakeChatClient::new();
        client.push_chat(Ok(read_main("call_0")));
        client.push_chat(Err(ChatError::Status {
            status: 401,
            body: "invalid api key".to_owned(),
        }));
        client.push_chat(Ok(reply("back again")));
        let cargo = FakeCargo::new();
        let tools = Tools::new(Root::new(dir.path()).unwrap(), &cargo);
        let mut agent = MainLine::new(&client, Router::new(&Config::default()), tools);
        let mut ui = ScriptedUi::new([input("add a test"), input("again"), input("/quit")]);

        let code = main_line(&mut ui, &mut agent);

        assert_eq!(code, ExitCode::SUCCESS);
        assert_eq!(
            ui.shown(),
            [
                "Stopped: the chat API returned HTTP 401: invalid api key",
                "Tool calls in this turn: read_file (1)",
                "back again",
            ]
        );
        assert_eq!(fs::read(dir.path().join("Cargo.toml")).unwrap(), before);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn going_over_the_budget_shows_the_stop_and_the_tool_calls_so_far() {
        let client = FakeChatClient::new();
        for i in 0..=TOOL_CALL_BUDGET {
            client.push_chat(Ok(read_main(&format!("call_{i}"))));
        }
        let (code, ui) = super::tests::run_script(&client, [input("read it"), input("/quit")]);

        assert_eq!(code, ExitCode::SUCCESS);
        assert_eq!(
            ui.shown(),
            [
                "Stopped: this turn reached the limit of 40 tool calls.",
                "Tool calls in this turn: read_file (40)",
            ]
        );
    }

    #[test]
    fn a_resend_that_is_still_broken_shows_why_the_turn_stopped() {
        let client = FakeChatClient::new();
        for id in ["first", "second"] {
            client.push_chat(Ok(ChatResponse {
                content: None,
                tool_calls: vec![ToolCall {
                    id: id.to_owned(),
                    name: "read_file".to_owned(),
                    arguments: "{".to_owned(),
                }],
            }));
        }
        let (_, ui) = super::tests::run_script(&client, [input("read it"), input("/quit")]);

        assert!(
            ui.shown()[0].starts_with(
                "Stopped: the model sent tool arguments that are not valid JSON again"
            ),
            "{:?}",
            ui.shown()
        );
    }
}

/// Approvals the tools ask for during a turn go to the user through the [`Ui`].
#[cfg(test)]
mod approval {
    use super::tests::{input, project, reply};
    use super::*;
    use crate::cli::ui::Step;
    use crate::llm::{ChatResponse, FakeChatClient, ToolCall};
    use crate::workspace::cargo::{CargoOutput, FakeCargo};

    #[test]
    fn approval_for_cargo_add_is_asked_through_the_ui() {
        let dir = project();
        let client = FakeChatClient::new();
        client.push_chat(Ok(ChatResponse {
            content: None,
            tool_calls: vec![ToolCall {
                id: "call_0".to_owned(),
                name: "run_cargo".to_owned(),
                arguments: r#"{"subcommand": "add", "args": ["serde"]}"#.to_owned(),
            }],
        }));
        client.push_chat(Ok(reply("not added")));
        let cargo = FakeCargo::new();
        cargo.push(Ok(CargoOutput {
            success: true,
            stdout: String::new(),
            stderr: String::new(),
        }));
        let tools = Tools::new(Root::new(dir.path()).unwrap(), &cargo);
        let mut agent = MainLine::new(&client, Router::new(&Config::default()), tools);
        let mut ui = ScriptedUi::new([input("add serde"), Step::Confirm(false), input("/quit")]);

        main_line(&mut ui, &mut agent);

        assert!(ui.asked()[1].contains("`serde`"), "{:?}", ui.asked());
        assert!(cargo.calls().is_empty(), "not approved, so not added");
    }
}
