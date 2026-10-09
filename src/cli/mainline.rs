//! `molt` without a subcommand: checks UC2's preconditions, then opens the main line in the cargo
//! project at cwd (PRD §8.6).

mod diff;

use std::path::Path;
use std::process::ExitCode;

use super::profile;
use super::ui::{Input, ScriptedUi, TerminalUi, Ui, UiApprover};
use crate::agent::mainline::{
    AddOutcome, Boundary, BoundaryCause, COMPILE_FIX_BUDGET, Change, CrateAdd, MainLine,
    TOOL_CALL_BUDGET, Turn, TurnEnd,
};
use crate::agent::tools::Tools;
use crate::config::{self, Config};
use crate::llm::{OpenAiClient, Router};
use crate::profile::status::ItemKey;
use crate::profile::store::Profile;
use crate::workspace::cargo::{Cargo, CargoError, CargoOutput};
use crate::workspace::path::Root;

/// The main-line prompt (PRD §8.6).
const PROMPT: &str = "molt> ";

/// The exit code after Ctrl-C at the prompt (128 + SIGINT, as a shell reports it).
const INTERRUPTED: u8 = 130;

pub(super) fn run(profile_flag: Option<&Path>, ui_script: Option<&Path>) -> ExitCode {
    let opened = preconditions(profile_flag).and_then(|(root, profile)| {
        let (config, client) = connect()?;
        let ui = open_ui(ui_script)?;
        Ok((root, profile, config, client, ui))
    });
    let (root, profile, config, client, mut ui) = match opened {
        Ok(opened) => opened,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };
    let cargo = Cargo::new(root.clone());
    let tools = Tools::new(root, &cargo);
    let mut agent = MainLine::new(&client, Router::new(&config), tools, profile);
    main_line(ui.as_mut(), &mut agent)
}

/// UC2's preconditions: a profile exists, and the current directory is a cargo project. Returns
/// that directory as the root the tools are confined to, and the profile.
fn preconditions(profile_flag: Option<&Path>) -> Result<(Root, Profile), String> {
    let path = profile::profile_path(profile_flag)?;
    let loaded = profile::load(&path)?;
    let cwd = std::env::current_dir()
        .map_err(|error| format!("cannot read the current directory: {error}"))?;
    if !cwd.join("Cargo.toml").is_file() {
        return Err(format!(
            "not a cargo project: no Cargo.toml in {}",
            cwd.display()
        ));
    }
    let root = Root::new(&cwd).map_err(|error| error.to_string())?;
    Ok((root, loaded))
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
                if matches!(turn.end, TurnEnd::Boundary(_))
                    && let Some(code) = at_boundary(ui, agent)
                {
                    return code;
                }
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

/// Shows what the model said last. When the turn stopped instead of finishing, also shows why,
/// the tool calls and the files written so far. Then the diff of each file molt changed and the
/// result of `cargo check` (PRD §3 Verification).
fn show_turn(ui: &mut dyn Ui, turn: &Turn) {
    if let Some(reply) = &turn.reply {
        ui.show(reply);
    }
    show_stop(ui, turn);
    // At a boundary the turn is held for the user's choice, not done: nothing was checked yet.
    if !matches!(turn.end, TurnEnd::Boundary(_)) {
        show_changes(ui, turn);
    }
}

/// Why the turn stopped, the tool calls and the files written so far, unless the model finished.
fn show_stop(ui: &mut dyn Ui, turn: &Turn) {
    let stopped = match &turn.end {
        TurnEnd::Finished => return,
        TurnEnd::OverBudget => {
            format!("Stopped: this turn reached the limit of {TOOL_CALL_BUDGET} tool calls.")
        }
        TurnEnd::InvalidArguments(error) => format!(
            "Stopped: the model sent tool arguments that are not valid JSON again after being asked to resend them: {error}"
        ),
        TurnEnd::ApiFailed(error) => format!("Stopped: {error}"),
        TurnEnd::Boundary(boundary) => boundary_text(boundary),
        TurnEnd::CompileFixesExhausted => format!(
            "Stopped: the code still does not build after {COMPILE_FIX_BUDGET} compile fixes in this turn."
        ),
    };
    ui.show(&stopped);
    if !turn.tool_calls.is_empty() {
        ui.show(&format!(
            "Tool calls in this turn: {}",
            count_by_name(&turn.tool_calls)
        ));
    }
    // Writes before the stop stay on disk; withdrawing the request does not undo them.
    if !turn.written.is_empty() {
        ui.show(&format!(
            "Files written in this turn: {}",
            turn.written.join(", ")
        ));
    }
}

/// The crates molt could not add, the diff of each file molt changed, and the result of the
/// `cargo check` molt ran at the end of the turn.
fn show_changes(ui: &mut dyn Ui, turn: &Turn) {
    show_crate_adds(ui, &turn.crate_adds);
    show_diffs_and_check(ui, &turn.changes, turn.check.as_ref());
}

fn show_crate_adds(ui: &mut dyn Ui, crate_adds: &[CrateAdd]) {
    for add in crate_adds {
        match &add.outcome {
            AddOutcome::Ran(output) if output.success => {}
            AddOutcome::Ran(output) => ui.show(&format!(
                "cargo add {} failed:\n{}",
                add.name,
                output.stderr.trim_end()
            )),
            AddOutcome::NotApproved => ui.show(&format!("Not added: the crate `{}`.", add.name)),
            AddOutcome::Failed(why) => ui.show(&format!("cargo add {} failed: {why}", add.name)),
        }
    }
}

/// The diff of each file in `changes`, then the result of `check`, if one ran.
fn show_diffs_and_check(
    ui: &mut dyn Ui,
    changes: &[Change],
    check: Option<&Result<CargoOutput, CargoError>>,
) {
    for change in changes {
        let diff = diff::unified(&change.path, change.before.as_deref(), &change.after);
        if !diff.is_empty() {
            ui.show(&diff);
        }
    }
    match check {
        None => {}
        Some(Ok(output)) if output.success => ui.show("cargo check passed."),
        Some(Ok(output)) => {
            let printed = [output.stdout.trim_end(), output.stderr.trim_end()]
                .into_iter()
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            ui.show(&format!("cargo check failed:\n{printed}"));
        }
        Some(Err(error)) => ui.show(&format!("cargo check could not run: {error}")),
    }
}

/// Why the turn stopped at `boundary`, and what the user can do there.
fn boundary_text(boundary: &Boundary) -> String {
    let items = boundary
        .items
        .iter()
        .map(|item| match item {
            ItemKey::Element(element) => format!("{item} ({})", element.category().name()),
            ItemKey::Crate(_) => item.to_string(),
        })
        .collect::<Vec<_>>()
        .join(", ");
    let why = match &boundary.cause {
        BoundaryCause::RejectedTwice { .. } => {
            "the model could not avoid them after a rewrite".to_owned()
        }
        BoundaryCause::Requested { reason, .. } => format!("the model says: {reason}"),
    };
    format!(
        "Stopped at a boundary: the request needs {items}, which your profile does not allow yet ({why}). Nothing was written for it."
    )
}

/// What the user can do at a boundary until the side line (UC3) is there.
const AT_BOUNDARY: &str = "Type /abort to withdraw the request.";

/// Reads lines at a boundary until `/abort`, which withdraws the request: nothing more is
/// changed, what was written before the boundary is shown with its `cargo check`, and the main
/// line waits for the next request. Any other line is not accepted. Ctrl-D and Ctrl-C end the
/// main line as at the prompt, and their exit code is returned.
fn at_boundary(ui: &mut dyn Ui, agent: &mut MainLine) -> Option<ExitCode> {
    ui.show(AT_BOUNDARY);
    loop {
        match ui.read_line(PROMPT) {
            Ok(Input::Line(line)) if line.trim() == "/abort" => {
                let withdrawal = agent.withdraw();
                ui.show("Request withdrawn.");
                show_diffs_and_check(ui, &withdrawal.changes, withdrawal.check.as_ref());
                return None;
            }
            Ok(Input::Line(_)) => ui.show(AT_BOUNDARY),
            Ok(Input::Eof) => return Some(ExitCode::SUCCESS),
            Ok(Input::Interrupted) => return Some(ExitCode::from(INTERRUPTED)),
            Err(error) => {
                eprintln!("{error}");
                return Some(ExitCode::FAILURE);
            }
        }
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
        let mut agent = MainLine::new(
            client,
            Router::new(&Config::default()),
            tools,
            Profile::new(),
        );
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
        let mut agent = MainLine::new(
            &client,
            Router::new(&Config::default()),
            tools,
            Profile::new(),
        );
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
        let mut agent = MainLine::new(
            &client,
            Router::new(&Config::default()),
            tools,
            Profile::new(),
        );
        let mut ui = ScriptedUi::new([input("add serde"), Step::Confirm(false), input("/quit")]);

        main_line(&mut ui, &mut agent);

        assert!(ui.asked()[1].contains("`serde`"), "{:?}", ui.asked());
        assert!(cargo.calls().is_empty(), "not approved, so not added");
    }
}

/// How the write path shows on the main line: a rejection stays between molt and the model, and
/// a boundary stops the main line until `/abort`. Named so that `cargo test mainline::write`
/// selects them.
#[cfg(test)]
mod write {
    use std::fs;

    use serde_json::json;

    use super::tests::{input, project, reply};
    use super::*;
    use crate::cli::ui::Step;
    use crate::llm::{ChatResponse, FakeChatClient, ToolCall};
    use crate::workspace::cargo::FakeCargo;

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

    const WITH_DBG: &str = "fn main() {\n    dbg!(1);\n}\n";

    /// A model that tries `dbg!` twice, which stops the first turn at a boundary, then answers
    /// the next request with `reply`.
    fn rejected_twice_then(reply_text: &str) -> FakeChatClient {
        let client = FakeChatClient::new();
        client.push_chat(Ok(write_main("first", WITH_DBG)));
        client.push_chat(Ok(write_main("second", WITH_DBG)));
        client.push_chat(Ok(reply(reply_text)));
        client
    }

    /// Runs the main line on `steps` in a project with `src/main.rs`, with the profile read from
    /// `profile.toml` in the project. Returns the exit code, the ui and the project.
    fn run(
        client: &FakeChatClient,
        steps: impl IntoIterator<Item = Step>,
    ) -> (ExitCode, ScriptedUi, tempfile::TempDir) {
        let dir = project();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(dir.path().join("profile.toml"), "").unwrap();
        let profile = Profile::load(&dir.path().join("profile.toml")).unwrap();
        let cargo = FakeCargo::new();
        let tools = Tools::new(Root::new(dir.path()).unwrap(), &cargo);
        let mut agent = MainLine::new(client, Router::new(&Config::default()), tools, profile);
        let mut ui = ScriptedUi::new(steps);
        let code = main_line(&mut ui, &mut agent);
        (code, ui, dir)
    }

    fn snapshot(dir: &std::path::Path) -> Vec<(String, Vec<u8>)> {
        let mut files = Vec::new();
        for path in ["Cargo.toml", "profile.toml", "src/main.rs"] {
            files.push((path.to_owned(), fs::read(dir.join(path)).unwrap()));
        }
        files.push((
            "entries".to_owned(),
            fs::read_dir(dir).unwrap().count().to_string().into_bytes(),
        ));
        files
    }

    #[test]
    fn a_boundary_is_shown_and_abort_leaves_the_files_and_the_profile_and_waits_for_the_next_input()
    {
        let client = rejected_twice_then("hello again");
        let (code, ui, dir) = run(
            &client,
            [
                input("debug 1"),
                input("/abort"),
                input("next"),
                input("/quit"),
            ],
        );

        assert_eq!(code, ExitCode::SUCCESS);
        assert_eq!(
            ui.shown(),
            [
                "Stopped at a boundary: the request needs dbg_macro (macros), which your profile does not allow yet (the model could not avoid them after a rewrite). Nothing was written for it.",
                "Tool calls in this turn: write_file (2)",
                "Type /abort to withdraw the request.",
                "Request withdrawn.",
                "hello again",
            ]
        );
        assert_eq!(ui.asked(), ["molt> "; 4]);
        assert_eq!(
            client.requests().len(),
            3,
            "the next input went to the model"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("src/main.rs")).unwrap(),
            "fn main() {}\n"
        );
        assert_eq!(fs::read(dir.path().join("profile.toml")).unwrap(), b"");
    }

    #[test]
    fn at_a_boundary_only_abort_is_accepted() {
        let client = rejected_twice_then("hello again");
        let (code, ui, _dir) = run(
            &client,
            [
                input("debug 1"),
                input("go on"),
                input("/quit"),
                input("/abort"),
                input("/quit"),
            ],
        );

        assert_eq!(code, ExitCode::SUCCESS);
        let shown = ui.shown();
        assert_eq!(
            shown[2..],
            [
                "Type /abort to withdraw the request.",
                "Type /abort to withdraw the request.",
                "Type /abort to withdraw the request.",
                "Request withdrawn.",
            ]
        );
        assert_eq!(
            client.requests().len(),
            2,
            "nothing went to the model at the boundary"
        );
    }

    #[test]
    fn abort_changes_nothing_that_the_boundary_left() {
        let client = rejected_twice_then("unused");
        let dir = project();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(dir.path().join("profile.toml"), "").unwrap();
        let before = snapshot(dir.path());
        let profile = Profile::load(&dir.path().join("profile.toml")).unwrap();
        let cargo = FakeCargo::new();
        let tools = Tools::new(Root::new(dir.path()).unwrap(), &cargo);
        let mut agent = MainLine::new(&client, Router::new(&Config::default()), tools, profile);
        let mut ui = ScriptedUi::new([input("debug 1"), input("/abort"), input("/quit")]);

        main_line(&mut ui, &mut agent);

        assert_eq!(snapshot(dir.path()), before);
        assert!(cargo.calls().is_empty());
    }

    #[test]
    fn a_rejection_is_not_shown_to_the_user() {
        let client = FakeChatClient::new();
        client.push_chat(Ok(write_main("first", WITH_DBG)));
        client.push_chat(Ok(write_main(
            "second",
            "fn main() {\n    println!(\"1\");\n}\n",
        )));
        client.push_chat(Ok(reply("Printed 1.")));
        let (_, ui, dir) = run(&client, [input("show 1"), input("/quit")]);

        assert_eq!(ui.shown()[0], "Printed 1.");
        assert!(
            ui.shown()
                .iter()
                .all(|text| !text.contains("rejected") && !text.contains("dbg")),
            "{:?}",
            ui.shown()
        );
        assert!(
            fs::read_to_string(dir.path().join("src/main.rs"))
                .unwrap()
                .contains("println!")
        );
    }

    #[test]
    fn a_requested_boundary_shows_the_reason_of_the_model() {
        let client = FakeChatClient::new();
        client.push_chat(Ok(ChatResponse {
            content: None,
            tool_calls: vec![ToolCall {
                id: "b".to_owned(),
                name: "request_boundary".to_owned(),
                arguments: json!({ "items": ["arc", "crate:tokio"], "reason": "tasks share it" })
                    .to_string(),
            }],
        }));
        let (_, ui, _dir) = run(&client, [input("share it"), Step::Eof]);

        assert_eq!(
            ui.shown()[0],
            "Stopped at a boundary: the request needs arc (smart_pointers), crate:tokio, which your profile does not allow yet (the model says: tasks share it). Nothing was written for it."
        );
    }

    #[test]
    fn files_written_before_a_boundary_are_shown_and_stay_after_abort() {
        let client = FakeChatClient::new();
        client.push_chat(Ok(ChatResponse {
            content: None,
            tool_calls: vec![ToolCall {
                id: "lib".to_owned(),
                name: "write_file".to_owned(),
                arguments: json!({ "path": "src/lib.rs", "content": "pub fn f() {}\n" })
                    .to_string(),
            }],
        }));
        client.push_chat(Ok(write_main("first", WITH_DBG)));
        client.push_chat(Ok(write_main("second", WITH_DBG)));
        let (_, ui, dir) = run(&client, [input("debug 1"), input("/abort"), Step::Eof]);

        assert_eq!(ui.shown()[2], "Files written in this turn: src/lib.rs");
        assert_eq!(
            fs::read_to_string(dir.path().join("src/lib.rs")).unwrap(),
            "pub fn f() {}\n"
        );
    }

    #[test]
    fn abort_shows_the_diff_and_the_check_of_what_was_written_before_the_boundary() {
        let client = FakeChatClient::new();
        client.push_chat(Ok(ChatResponse {
            content: None,
            tool_calls: vec![ToolCall {
                id: "lib".to_owned(),
                name: "write_file".to_owned(),
                arguments: json!({ "path": "src/lib.rs", "content": "pub fn f() {}\n" })
                    .to_string(),
            }],
        }));
        client.push_chat(Ok(write_main("first", WITH_DBG)));
        client.push_chat(Ok(write_main("second", WITH_DBG)));
        let (_, ui, _dir) = run(&client, [input("debug 1"), input("/abort"), Step::Eof]);

        let shown = ui.shown();
        let at = shown
            .iter()
            .position(|text| text == "Request withdrawn.")
            .unwrap();
        assert_eq!(
            shown[at + 1..],
            [
                "--- /dev/null\n+++ b/src/lib.rs\n@@ -0,0 +1 @@\n+pub fn f() {}",
                "cargo check could not run: the fake cargo has no more queued results",
            ]
        );
        assert!(
            shown[..at].iter().all(|text| !text.starts_with("---")),
            "no diff at the boundary itself: {shown:?}"
        );
    }

    #[test]
    fn ctrl_d_at_a_boundary_ends_the_main_line_with_success() {
        let client = rejected_twice_then("unused");
        let (code, _, _dir) = run(&client, [input("debug 1"), Step::Eof]);
        assert_eq!(code, ExitCode::SUCCESS);
    }

    #[test]
    fn ctrl_c_at_a_boundary_ends_the_main_line_with_a_non_zero_code() {
        let client = rejected_twice_then("unused");
        let (code, _, _dir) = run(&client, [input("debug 1"), Step::Interrupt]);
        assert_eq!(code, ExitCode::from(INTERRUPTED));
    }
}

/// How the end of a turn that wrote shows on the main line: the diff of each file molt changed,
/// the result of `cargo check`, and the approval of `cargo add`. Named so that
/// `cargo test mainline::turn_end` selects them.
#[cfg(test)]
mod turn_end {
    use std::fs;

    use serde_json::json;

    use super::tests::{input, project, reply};
    use super::*;
    use crate::cli::ui::Step;
    use crate::llm::{ChatResponse, FakeChatClient, ToolCall};
    use crate::profile::status::{Provenance, Status};
    use crate::profile::store::Timestamp;
    use crate::workspace::cargo::{CargoCall, CargoOutput, CrateName, FakeCargo, Subcommand};

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

    fn output(success: bool, stderr: &str) -> CargoOutput {
        CargoOutput {
            success,
            stdout: String::new(),
            stderr: stderr.to_owned(),
        }
    }

    /// Runs the main line on `steps` in a project with `src/main.rs`.
    fn run(
        client: &FakeChatClient,
        cargo: &FakeCargo,
        profile: Profile,
        steps: impl IntoIterator<Item = Step>,
    ) -> ScriptedUi {
        let dir = project();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        let tools = Tools::new(Root::new(dir.path()).unwrap(), cargo);
        let mut agent = MainLine::new(client, Router::new(&Config::default()), tools, profile);
        let mut ui = ScriptedUi::new(steps);
        main_line(&mut ui, &mut agent);
        ui
    }

    const PRINT_1: &str = "fn main() {\n    println!(\"1\");\n}\n";
    const DIFF: &str = "--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1 +1,3 @@\n-fn main() {}\n+fn main() {\n+    println!(\"1\");\n+}";

    #[test]
    fn the_diff_of_the_written_file_and_the_passed_check_are_shown_after_the_reply() {
        let client = FakeChatClient::new();
        client.push_chat(Ok(write_main("w", PRINT_1)));
        client.push_chat(Ok(reply("Printed 1.")));
        let cargo = FakeCargo::new();
        cargo.push(Ok(output(true, "")));

        let ui = run(
            &client,
            &cargo,
            Profile::new(),
            [input("print 1"), input("/quit")],
        );

        assert_eq!(ui.shown(), ["Printed 1.", DIFF, "cargo check passed."]);
    }

    #[test]
    fn a_failed_check_is_shown_with_its_errors() {
        let client = FakeChatClient::new();
        client.push_chat(Ok(write_main("w", PRINT_1)));
        client.push_chat(Ok(reply("Printed 1.")));
        client.push_chat(Ok(reply("I could not fix it.")));
        let cargo = FakeCargo::new();
        cargo.push(Ok(output(false, "error[E0308]: mismatched types\n")));

        let ui = run(
            &client,
            &cargo,
            Profile::new(),
            [input("print 1"), input("/quit")],
        );

        assert_eq!(
            ui.shown(),
            [
                "I could not fix it.",
                DIFF,
                "cargo check failed:\nerror[E0308]: mismatched types"
            ]
        );
    }

    #[test]
    fn going_over_the_compile_fixes_shows_the_stop_the_state_and_the_last_errors() {
        let client = FakeChatClient::new();
        let cargo = FakeCargo::new();
        for i in 0..=COMPILE_FIX_BUDGET {
            client.push_chat(Ok(write_main(&format!("w{i}"), PRINT_1)));
            client.push_chat(Ok(reply("done")));
            cargo.push(Ok(output(false, &format!("error: attempt {i}\n"))));
        }

        let ui = run(
            &client,
            &cargo,
            Profile::new(),
            [input("print 1"), input("/quit")],
        );

        assert_eq!(
            ui.shown(),
            [
                "done",
                "Stopped: the code still does not build after 3 compile fixes in this turn.",
                "Tool calls in this turn: write_file (4)",
                "Files written in this turn: src/main.rs",
                DIFF,
                "cargo check failed:\nerror: attempt 3",
            ]
        );
    }

    #[test]
    fn a_check_that_cannot_run_is_shown() {
        let client = FakeChatClient::new();
        client.push_chat(Ok(write_main("w", PRINT_1)));
        client.push_chat(Ok(reply("Printed 1.")));

        let ui = run(
            &client,
            &FakeCargo::new(),
            Profile::new(),
            [input("print 1"), input("/quit")],
        );

        assert_eq!(
            ui.shown().last().unwrap(),
            "cargo check could not run: the fake cargo has no more queued results"
        );
    }

    fn known_serde_json() -> Profile {
        let mut profile = Profile::new();
        profile
            .transition(
                ItemKey::Crate("serde_json".to_owned()),
                Status::Known,
                Provenance::Manual,
                Timestamp::from_unix_seconds(0),
                None,
            )
            .unwrap();
        profile
    }

    const USES_SERDE_JSON: &str = "fn main() {\n    let _ = serde_json::to_string(&1);\n}\n";

    #[test]
    fn approving_a_known_crate_missing_from_cargo_toml_adds_it() {
        let client = FakeChatClient::new();
        client.push_chat(Ok(write_main("w", USES_SERDE_JSON)));
        client.push_chat(Ok(reply("done")));
        let cargo = FakeCargo::new();
        cargo.push(Ok(output(true, "")));
        cargo.push(Ok(output(true, "")));

        let ui = run(
            &client,
            &cargo,
            known_serde_json(),
            [input("serialize 1"), Step::Confirm(true), input("/quit")],
        );

        assert_eq!(
            cargo.calls(),
            [
                CargoCall::Add(CrateName::new("serde_json").unwrap()),
                CargoCall::Run(Subcommand::Check),
            ]
        );
        assert!(ui.asked()[1].contains("`serde_json`"), "{:?}", ui.asked());
    }

    #[test]
    fn refusing_a_known_crate_missing_from_cargo_toml_does_not_add_it() {
        let client = FakeChatClient::new();
        client.push_chat(Ok(write_main("w", USES_SERDE_JSON)));
        client.push_chat(Ok(reply("done")));
        client.push_chat(Ok(reply("serde_json is not a dependency")));
        let cargo = FakeCargo::new();
        cargo.push(Ok(output(false, "error[E0433]: unresolved crate\n")));

        let ui = run(
            &client,
            &cargo,
            known_serde_json(),
            [input("serialize 1"), Step::Confirm(false), input("/quit")],
        );

        assert_eq!(cargo.calls(), [CargoCall::Run(Subcommand::Check)]);
        assert!(
            ui.shown()
                .contains(&"Not added: the crate `serde_json`.".to_owned()),
            "{:?}",
            ui.shown()
        );
    }
}
