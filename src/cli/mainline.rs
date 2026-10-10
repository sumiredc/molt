//! `molt` without a subcommand: checks UC2's preconditions, then opens the main line in the cargo
//! project at cwd (PRD §8.6).

pub(super) mod commit;
mod diff;
mod subline;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use self::subline::Left;
use super::profile;
use super::ui::{Input, ScriptedUi, TerminalUi, Ui, UiApprover};
use crate::agent::mainline::{
    AddOutcome, Boundary, BoundaryCause, COMPILE_FIX_BUDGET, Change, CrateAdd, MainLine, Resolved,
    TOOL_CALL_BUDGET, Turn, TurnEnd,
};
use crate::agent::subline::{Choice, Resolution, Unresolved};
use crate::agent::tools::Tools;
use crate::config::{self, Config};
use crate::llm::{ChatClient, Interrupt, OpenAiClient, Router, fake_script};
use crate::profile::status::ItemKey;
use crate::profile::store::Profile;
use crate::session::{self, SessionLog};
use crate::workspace::cargo::{Cargo, CargoError, CargoOutput};
use crate::workspace::path::Root;

/// The main-line prompt (PRD §8.6).
const PROMPT: &str = "molt> ";

/// The directory in molt's place (`config::molt_home`) that holds the session logs (PRD §8.8).
const SESSIONS_DIR: &str = "sessions";

/// The exit code after Ctrl-C at the prompt (128 + SIGINT, as a shell reports it).
const INTERRUPTED: u8 = 130;

pub(super) fn run(
    profile_flag: Option<&Path>,
    fake: Option<&Path>,
    ui_script: Option<&Path>,
) -> ExitCode {
    let opened = preconditions(profile_flag).and_then(|(root, profile, profile_path)| {
        let (home, config, client) = connect(fake)?;
        let ui = open_ui(ui_script)?;
        let log = open_log(&home)?;
        Ok((root, profile, profile_path, config, client, ui, log))
    });
    let (root, profile, profile_path, config, client, mut ui, log) = match opened {
        Ok(opened) => opened,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };
    let cargo = Cargo::new(root.clone());
    let tools = Tools::new(root, &cargo);
    let mut agent = MainLine::new(client.as_ref(), Router::new(&config), tools, profile)
        .with_log(log)
        .with_profile_path(profile_path);
    main_line(ui.as_mut(), &mut agent)
}

/// UC2's preconditions: a profile exists, and the current directory is a cargo project. Returns
/// that directory as the root the tools are confined to, the profile, and where it is saved.
fn preconditions(profile_flag: Option<&Path>) -> Result<(Root, Profile, PathBuf), String> {
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
    Ok((root, loaded, path))
}

/// Reads `config.toml` and creates the client: the responses of the `--fake` script at `fake`,
/// otherwise the configured endpoint, whose request Ctrl-C drops while molt waits for it (the
/// script answers at once, so it is not waited for). The main line does not open when the client
/// cannot be created, e.g. the script cannot be read or the API key's environment variable is
/// unset.
fn connect(fake: Option<&Path>) -> Result<(PathBuf, Config, Box<dyn ChatClient>), String> {
    let home = config::molt_home(|name| std::env::var_os(name)).map_err(|e| e.to_string())?;
    let config = Config::load(&config::config_path(&home)).map_err(|e| e.to_string())?;
    let client: Box<dyn ChatClient> = match fake {
        Some(path) => Box::new(fake_script::load(path).map_err(|e| e.to_string())?),
        None => {
            let client = OpenAiClient::new(&config.api, |name| std::env::var(name).ok())
                .map_err(|e| e.to_string())?;
            // Before the first line is read: rustyline restores the SIGINT action it found.
            match Interrupt::on_sigint() {
                Ok(interrupt) => Box::new(client.with_interrupt(interrupt)),
                Err(error) => {
                    eprintln!(
                        "cannot catch Ctrl-C while waiting for the model ({error}); Ctrl-C then ends molt"
                    );
                    Box::new(client)
                }
            }
        }
    };
    Ok((home, config, client))
}

/// Creates the session log in the `sessions` directory of molt's place `home`, beside the
/// profile. The main line does not open without it, since what it records is a guarantee
/// (PRD §3 Observability).
fn open_log(home: &Path) -> Result<SessionLog, String> {
    let dir = home.join(SESSIONS_DIR);
    let request_bodies = session::request_bodies_enabled(|name| std::env::var_os(name));
    SessionLog::create(&dir, request_bodies).map_err(|error| {
        format!(
            "cannot create the session log in {}: {error}",
            dir.display()
        )
    })
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

/// Reads requests until `/quit` or Ctrl-D (exit 0) or Ctrl-C at the prompt (exit non-zero), and runs a turn
/// of `agent` for each. A blank line is not sent to the model. After `/quit` and Ctrl-D, shows
/// where the session log is.
fn main_line(ui: &mut dyn Ui, agent: &mut MainLine) -> ExitCode {
    let code = requests(ui, agent);
    // Only `/quit` and Ctrl-D, at the prompt, on the side line or while paused, end with success.
    if code == ExitCode::SUCCESS {
        show_where_logged(ui, agent.log());
    }
    code
}

/// The loop of [`main_line`], which returns as soon as the main line ends.
fn requests(ui: &mut dyn Ui, agent: &mut MainLine) -> ExitCode {
    loop {
        match ui.read_line(PROMPT) {
            Ok(Input::Line(line)) if line.trim() == "/quit" => return ExitCode::SUCCESS,
            Ok(Input::Line(line)) if line.trim().is_empty() => {}
            Ok(Input::Line(line)) => {
                let turn = agent.turn(&line, &mut UiApprover::new(ui));
                show_turn(ui, &turn);
                show_log_error(ui, agent);
                if let TurnEnd::Boundary(boundary) = turn.end {
                    if let Some(code) = at_boundary(ui, agent, boundary) {
                        return code;
                    }
                } else {
                    commit::offer(ui, agent, &turn);
                    show_log_error(ui, agent);
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
/// result of `cargo check` (PRD §3 Verification), and what came of each commit the model asked
/// for (UC4).
fn show_turn(ui: &mut dyn Ui, turn: &Turn) {
    if let Some(reply) = &turn.reply {
        ui.show(reply);
    }
    show_stop(ui, turn);
    // At a boundary the turn is held for the user's choice, not done: nothing was checked yet.
    if !matches!(turn.end, TurnEnd::Boundary(_)) {
        show_changes(ui, turn);
    }
    for outcome in &turn.commits {
        commit::show_outcome(ui, outcome);
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

/// What the user can do while the main line is paused at a boundary (UC3's exception flow).
const PAUSED: &str = "Paused at the boundary. Type /side to go back to the side line, or /abort to withdraw the request.";

/// Opens the side line (UC3) at `boundary`, where the turn stopped. A choice there goes on with
/// the turn, which opens the side line again when it stops at another boundary. `/abort` there
/// withdraws the request. Ctrl-C there pauses the main line at the boundary, where `/side` opens the same side
/// line again and `/abort` withdraws the request, and no other line is accepted. Ctrl-D on the
/// side line, and Ctrl-D and Ctrl-C while paused, end the main line as at the prompt, and their
/// exit code is returned.
fn at_boundary(ui: &mut dyn Ui, agent: &mut MainLine, boundary: Boundary) -> Option<ExitCode> {
    let mut side = agent.side_line(boundary);
    loop {
        match subline::run(ui, &mut side, agent) {
            Left::Aborted => {
                withdraw(ui, agent);
                return None;
            }
            Left::Chose(choice) => match side.resolve(choice, agent.log()) {
                Ok(resolution) => {
                    // When the turn went on to its end, the main line takes the next request
                    // (`None`). At another boundary, the side line opens there.
                    let boundary = resolve(ui, agent, &resolution)?;
                    side = agent.side_line(boundary);
                    continue;
                }
                // Ctrl-C while waiting for the policy of ②: nothing changed, so the main line is
                // paused at the boundary as after Ctrl-C on the side line.
                Err(Unresolved::Interrupted) => show_log_error(ui, agent),
                // `subline::run` offers only the choices of `side`.
                Err(Unresolved::NotOffered) => return None,
            },
            Left::Wrote(resolution) => {
                // As after ① or ②.
                let boundary = resolve(ui, agent, &resolution)?;
                side = agent.side_line(boundary);
                continue;
            }
            Left::Interrupted => {}
            Left::Eof => return Some(ExitCode::SUCCESS),
            Left::Failed => return Some(ExitCode::FAILURE),
        }
        ui.show(PAUSED);
        loop {
            match ui.read_line(PROMPT) {
                Ok(Input::Line(line)) if line.trim() == "/side" => break,
                Ok(Input::Line(line)) if line.trim() == "/abort" => {
                    withdraw(ui, agent);
                    return None;
                }
                Ok(Input::Line(_)) => ui.show(PAUSED),
                Ok(Input::Eof) => return Some(ExitCode::SUCCESS),
                Ok(Input::Interrupted) => return Some(ExitCode::from(INTERRUPTED)),
                Err(error) => {
                    eprintln!("{error}");
                    return Some(ExitCode::FAILURE);
                }
            }
        }
    }
}

/// Hands the choice made on the side line to the main line (UC3, §8.5). While items of the
/// boundary still wait for a choice, says so and returns them for the next side line. Once every
/// item has one, shows how each choice changed the profile, then the rest of the turn as any turn,
/// and returns the next boundary when the turn stops at one again.
fn resolve(ui: &mut dyn Ui, agent: &mut MainLine, resolution: &Resolution) -> Option<Boundary> {
    let resumed = match agent.resolve(resolution, &mut UiApprover::new(ui))? {
        Resolved::Pending(pending) => {
            ui.show(&format!(
                "Chosen for {}: {}. The main line goes on once there is a choice for {} too.",
                names(resolution.answers()),
                chosen(resolution.choice()),
                names(&pending.items)
            ));
            show_log_error(ui, agent);
            return Some(pending);
        }
        Resolved::Resumed(resumed) => resumed,
    };
    for resolution in &resumed.chosen {
        let names = names(resolution.items());
        match (resolution.choice(), resolution.policy()) {
            (Choice::Rewrite, Some(policy)) => ui.show(&format!(
                "Your profile marks {names} as seen, and molt rewrites without them. Policy: {policy}"
            )),
            (Choice::UserWrote, _) if names.is_empty() => {
                ui.show("Your lines use nothing your profile did not allow already.")
            }
            (Choice::UserWrote, _) => ui.show(&format!(
                "Your profile allows {names} now (known: you wrote it)."
            )),
            _ => ui.show(&format!("Your profile allows {names} now (known).")),
        }
    }
    if let Err(error) = &resumed.saved {
        ui.show(&format!(
            "Could not save the profile: {error}. The change holds for this session only."
        ));
    }
    show_turn(ui, &resumed.turn);
    show_log_error(ui, agent);
    match resumed.turn.end {
        TurnEnd::Boundary(boundary) => Some(boundary),
        _ => {
            commit::offer(ui, agent, &resumed.turn);
            show_log_error(ui, agent);
            None
        }
    }
}

/// `arc, mutex`.
fn names(items: &[ItemKey]) -> String {
    items
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// What the user chose, in the words of the choices on the side line.
fn chosen(choice: Choice) -> &'static str {
    match choice {
        Choice::Unlock => "understand, then delegate",
        Choice::Rewrite => "use a construct I know",
        Choice::UserWrote => "write it myself",
    }
}

/// Withdraws the request held at a boundary: nothing more is changed, what was written before the
/// boundary is shown with its `cargo check`, and the main line waits for the next request.
fn withdraw(ui: &mut dyn Ui, agent: &mut MainLine) {
    let withdrawal = agent.withdraw();
    ui.show("Request withdrawn.");
    show_diffs_and_check(ui, &withdrawal.changes, withdrawal.check.as_ref());
    show_log_error(ui, agent);
}

/// Where `log` wrote this session, and the request bodies when `MOLT_LOG` enabled them, so that
/// the user can trace it later (PRD §8.8). Nothing when the log is disabled.
fn show_where_logged(ui: &mut dyn Ui, log: &SessionLog) {
    if let Some(path) = log.path() {
        ui.show(&format!("Session log: {}", path.display()));
    }
    if let Some(path) = log.requests_path() {
        ui.show(&format!("Request bodies (MOLT_LOG): {}", path.display()));
    }
}

/// That the session log could not be written, the first time it happens to a file. The main line
/// goes on without it.
fn show_log_error(ui: &mut dyn Ui, agent: &MainLine) {
    show_error_of(ui, agent.log());
}

/// That `log` could not be written, the first time it happens to a file.
fn show_error_of(ui: &mut dyn Ui, log: &SessionLog) {
    if let Some((path, error)) = log.take_error() {
        ui.show(&format!(
            "Could not write the session log {}: {error}. The session goes on without it.",
            path.display()
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

    /// A model that tries `dbg!` twice, which stops the first turn at a boundary, explains the
    /// idea on the side line, then answers the next request with `reply`.
    fn rejected_twice_then(reply_text: &str) -> FakeChatClient {
        let client = FakeChatClient::new();
        client.push_chat(Ok(write_main("first", WITH_DBG)));
        client.push_chat(Ok(write_main("second", WITH_DBG)));
        client.push_chat(Ok(reply("dbg! prints a value.")));
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
    fn a_boundary_opens_the_side_line_and_abort_there_leaves_the_files_and_the_profile_and_waits_for_the_next_input()
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
                "── side line: dbg_macro ────────────────────────────────────",
                "  dbg! prints a value.\n\n  Two ways to write it\n  With `dbg_macro` (what the main line tried):\n  ```rust\n  fn main() {\n      dbg!(1);\n  }\n  ```\n  Without them, within your profile: hard to write. The model found no good way to.",
                "  Ask anything about it, as many times as you like. Type /choose to choose how to go on, or /abort to withdraw the request.",
                "── back to the main line ───────────────────────────────────",
                "Request withdrawn.",
                "hello again",
            ]
        );
        assert_eq!(
            ui.asked(),
            ["molt> ", "molt [sub: dbg_macro]> ", "molt> ", "molt> "]
        );
        assert_eq!(
            client.requests().len(),
            4,
            "the explanation and the next input went to the model"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("src/main.rs")).unwrap(),
            "fn main() {}\n"
        );
        assert_eq!(fs::read(dir.path().join("profile.toml")).unwrap(), b"");
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
    fn ctrl_d_on_the_side_line_at_a_boundary_ends_the_main_line_with_success() {
        let client = rejected_twice_then("unused");
        let (code, _, _dir) = run(&client, [input("debug 1"), Step::Eof]);
        assert_eq!(code, ExitCode::SUCCESS);
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

/// What the main line leaves in the session log (PRD §3 Observability, §8.8). Named so that
/// `cargo test session` selects them.
#[cfg(test)]
mod session_log {
    use std::fs;

    use serde_json::{Value, json};

    use super::tests::{input, project};
    use super::*;
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

    #[test]
    fn a_run_from_a_rejection_to_a_boundary_leaves_the_four_kinds_of_events_in_the_session_log() {
        let dir = project();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        let logs = tempfile::tempdir().unwrap();
        let client = FakeChatClient::new();
        // A clean write, then `dbg!` twice: the second rejection is a boundary.
        client.push_chat(Ok(write_main(
            "clean",
            "fn main() {\n    println!(\"1\");\n}\n",
        )));
        client.push_chat(Ok(write_main("first", WITH_DBG)));
        client.push_chat(Ok(write_main("second", WITH_DBG)));
        let cargo = FakeCargo::new();
        cargo.push(Ok(CargoOutput {
            success: true,
            stdout: String::new(),
            stderr: "Finished".to_owned(),
        }));
        let tools = Tools::new(Root::new(dir.path()).unwrap(), &cargo);
        let log = SessionLog::create(logs.path(), false).unwrap();
        let path = log.path().unwrap().to_owned();
        let mut agent = MainLine::new(
            &client,
            Router::new(&Config::default()),
            tools,
            Profile::new(),
        )
        .with_log(log);
        let mut ui = ScriptedUi::new([input("debug 1"), input("/abort"), input("/quit")]);

        assert_eq!(main_line(&mut ui, &mut agent), ExitCode::SUCCESS);

        let events: Vec<Value> = fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let kinds: Vec<&str> = events
            .iter()
            .map(|event| event["event"].as_str().unwrap())
            .collect();
        assert_eq!(
            kinds,
            [
                "tool_call",
                "tool_call",
                "detected",
                "rejected",
                "tool_call",
                "detected",
                "rejected",
                "cargo_check",
            ]
        );
        assert_eq!(events[0]["name"], "write_file");
        assert_eq!(events[2]["path"], "src/main.rs");
        assert_eq!(
            events[2]["items"],
            json!([{ "line": 2, "item": "dbg_macro", "category": "macros", "text": "dbg!" }])
        );
        assert_eq!(events[3]["items"], json!(["dbg_macro"]));
        assert_eq!(events[3]["content"], WITH_DBG);
        assert_eq!(events[3]["boundary"], false);
        assert_eq!(events[6]["boundary"], true);
        assert_eq!(events[7]["status"], "passed");
        assert!(
            events.iter().all(|event| event.get("line").is_none()),
            "main-line events carry no `line`"
        );
    }

    #[test]
    fn the_cargo_check_at_the_end_of_a_turn_is_in_the_session_log_with_its_errors() {
        let dir = project();
        fs::create_dir(dir.path().join("src")).unwrap();
        let logs = tempfile::tempdir().unwrap();
        let client = FakeChatClient::new();
        client.push_chat(Ok(write_main("write", "fn main() {}\n")));
        client.push_chat(Err(crate::llm::ChatError::FakeExhausted));
        let cargo = FakeCargo::new();
        cargo.push(Ok(CargoOutput {
            success: false,
            stdout: String::new(),
            stderr: "error[E0425]".to_owned(),
        }));
        let tools = Tools::new(Root::new(dir.path()).unwrap(), &cargo);
        let log = SessionLog::create(logs.path(), false).unwrap();
        let path = log.path().unwrap().to_owned();
        let mut agent = MainLine::new(
            &client,
            Router::new(&Config::default()),
            tools,
            Profile::new(),
        )
        .with_log(log);
        let mut ui = ScriptedUi::new([input("write main"), input("/quit")]);

        main_line(&mut ui, &mut agent);

        let text = fs::read_to_string(&path).unwrap();
        let last: Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
        assert_eq!(last["event"], "cargo_check");
        assert_eq!(last["status"], "failed");
        assert_eq!(last["stderr"], "error[E0425]");
    }

    #[test]
    fn a_session_log_that_cannot_be_written_is_shown_once_and_the_main_line_goes_on() {
        let dir = project();
        let logs = tempfile::tempdir().unwrap();
        let path = logs.path().join("read-only.jsonl");
        fs::write(&path, "").unwrap();
        let client = FakeChatClient::new();
        let read = |id: &str| ChatResponse {
            content: None,
            tool_calls: vec![ToolCall {
                id: id.to_owned(),
                name: "read_file".to_owned(),
                arguments: r#"{"path": "Cargo.toml"}"#.to_owned(),
            }],
        };
        client.push_chat(Ok(read("first")));
        client.push_chat(Ok(super::tests::reply("one")));
        client.push_chat(Ok(read("second")));
        client.push_chat(Ok(super::tests::reply("two")));
        let cargo = FakeCargo::new();
        let tools = Tools::new(Root::new(dir.path()).unwrap(), &cargo);
        let mut agent = MainLine::new(
            &client,
            Router::new(&Config::default()),
            tools,
            Profile::new(),
        )
        .with_log(SessionLog::unwritable(&path));
        let mut ui = ScriptedUi::new([input("first"), input("second"), input("/quit")]);

        assert_eq!(main_line(&mut ui, &mut agent), ExitCode::SUCCESS);

        let warning = format!("Could not write the session log {}: ", path.display());
        let shown = ui.shown();
        assert_eq!(shown.len(), 4, "{shown:?}");
        assert_eq!(shown[0], "one");
        assert!(shown[1].starts_with(&warning), "{shown:?}");
        assert_eq!(shown[2], "two");
        assert_eq!(shown[3], format!("Session log: {}", path.display()));
    }
}

/// Where the session log is, shown when the main line ends with `/quit` or Ctrl-D (PRD §8.8).
/// Named so that `cargo test session_path` selects them.
#[cfg(test)]
mod session_path {
    use std::fs;
    use std::path::PathBuf;

    use serde_json::json;
    use tempfile::TempDir;

    use super::tests::{input, project};
    use super::*;
    use crate::cli::ui::Step;
    use crate::llm::{ChatResponse, FakeChatClient, ToolCall};
    use crate::workspace::cargo::FakeCargo;

    /// What the main line showed on `steps` with a session log in a tempdir, and the paths of
    /// the log's events and request bodies.
    struct Ran {
        code: ExitCode,
        shown: Vec<String>,
        events: PathBuf,
        requests: Option<PathBuf>,
        _dirs: (TempDir, TempDir),
    }

    fn run(
        client: &FakeChatClient,
        request_bodies: bool,
        steps: impl IntoIterator<Item = Step>,
    ) -> Ran {
        let dir = project();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        let logs = tempfile::tempdir().unwrap();
        let log = SessionLog::create(logs.path(), request_bodies).unwrap();
        let events = log.path().unwrap().to_owned();
        let requests = log.requests_path().map(Path::to_owned);
        let cargo = FakeCargo::new();
        let tools = Tools::new(Root::new(dir.path()).unwrap(), &cargo);
        let mut agent = MainLine::new(
            client,
            Router::new(&Config::default()),
            tools,
            Profile::new(),
        )
        .with_log(log);
        let mut ui = ScriptedUi::new(steps);
        let code = main_line(&mut ui, &mut agent);
        Ran {
            code,
            shown: ui.shown().to_vec(),
            events,
            requests,
            _dirs: (dir, logs),
        }
    }

    fn session_line(ran: &Ran) -> String {
        format!("Session log: {}", ran.events.display())
    }

    #[test]
    fn quit_shows_the_path_of_the_session_log_last() {
        let ran = run(&FakeChatClient::new(), false, [input("/quit")]);
        assert_eq!(ran.code, ExitCode::SUCCESS);
        assert_eq!(ran.shown, [session_line(&ran)]);
        assert!(ran.events.is_file(), "the shown path is the log's file");
    }

    #[test]
    fn ctrl_d_at_the_prompt_shows_the_path_of_the_session_log() {
        let ran = run(&FakeChatClient::new(), false, [Step::Eof]);
        assert_eq!(ran.code, ExitCode::SUCCESS);
        assert_eq!(ran.shown, [session_line(&ran)]);
    }

    #[test]
    fn ctrl_d_on_the_side_line_at_a_boundary_shows_the_path_of_the_session_log_last() {
        let ran = run(&rejected_twice(), false, [input("debug 1"), Step::Eof]);
        assert_eq!(ran.code, ExitCode::SUCCESS);
        assert_eq!(
            ran.shown.last(),
            Some(&session_line(&ran)),
            "{:?}",
            ran.shown
        );
    }

    #[test]
    fn ctrl_d_while_paused_at_a_boundary_shows_the_path_of_the_session_log_last() {
        let ran = run(
            &rejected_twice(),
            false,
            [input("debug 1"), Step::Interrupt, Step::Eof],
        );
        assert_eq!(ran.code, ExitCode::SUCCESS);
        assert!(ran.shown.contains(&PAUSED.to_owned()), "{:?}", ran.shown);
        assert_eq!(
            ran.shown.last(),
            Some(&session_line(&ran)),
            "{:?}",
            ran.shown
        );
    }

    /// A client whose model writes `dbg!` twice: the turn stops at a boundary.
    fn rejected_twice() -> FakeChatClient {
        let client = FakeChatClient::new();
        let with_dbg = |id: &str| ChatResponse {
            content: None,
            tool_calls: vec![ToolCall {
                id: id.to_owned(),
                name: "write_file".to_owned(),
                arguments:
                    json!({ "path": "src/main.rs", "content": "fn main() {\n    dbg!(1);\n}\n" })
                        .to_string(),
            }],
        };
        client.push_chat(Ok(with_dbg("first")));
        client.push_chat(Ok(with_dbg("second")));
        client
    }

    #[test]
    fn with_molt_log_the_path_of_the_request_bodies_follows_the_session_log() {
        let ran = run(&FakeChatClient::new(), true, [input("/quit")]);
        let requests = ran.requests.as_ref().unwrap();
        assert_eq!(
            ran.shown,
            [
                session_line(&ran),
                format!("Request bodies (MOLT_LOG): {}", requests.display()),
            ]
        );
    }

    #[test]
    fn ctrl_c_at_the_prompt_does_not_show_the_path() {
        let ran = run(&FakeChatClient::new(), false, [Step::Interrupt]);
        assert_eq!(ran.code, ExitCode::from(INTERRUPTED));
        assert!(ran.shown.is_empty(), "{:?}", ran.shown);
    }

    #[test]
    fn a_ui_error_does_not_show_the_path() {
        let ran = run(&FakeChatClient::new(), false, []);
        assert_eq!(ran.code, ExitCode::FAILURE);
        assert!(ran.shown.is_empty(), "{:?}", ran.shown);
    }

    #[test]
    fn a_disabled_log_shows_no_path() {
        let client = FakeChatClient::new();
        let (code, ui) = super::tests::run_script(&client, [input("/quit")]);
        assert_eq!(code, ExitCode::SUCCESS);
        assert!(ui.shown().is_empty(), "{:?}", ui.shown());
    }
}

/// Ctrl-C while the main line waits for the model: the turn stops there, what was written stays,
/// and the prompt comes back (PRD §8.1). The client returns [`ChatError::Interrupted`] as the real
/// one does on Ctrl-C. Named so that `cargo test while_waiting` selects them.
#[cfg(test)]
mod while_waiting {
    use std::fs;

    use serde_json::json;

    use super::tests::{input, project, reply};
    use super::*;
    use crate::llm::{ChatError, ChatResponse, FakeChatClient, ToolCall};
    use crate::workspace::cargo::{CargoOutput, FakeCargo};

    const PRINT_1: &str = "fn main() {\n    println!(\"1\");\n}\n";

    #[test]
    fn an_interruption_stops_the_turn_keeps_what_was_written_and_the_prompt_comes_back() {
        let dir = project();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        let client = FakeChatClient::new();
        client.push_chat(Ok(ChatResponse {
            content: None,
            tool_calls: vec![ToolCall {
                id: "w".to_owned(),
                name: "write_file".to_owned(),
                arguments: json!({ "path": "src/main.rs", "content": PRINT_1 }).to_string(),
            }],
        }));
        client.push_chat(Err(ChatError::Interrupted));
        client.push_chat(Ok(reply("back again")));
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
        let mut ui = ScriptedUi::new([input("print 1"), input("again"), input("/quit")]);

        let code = main_line(&mut ui, &mut agent);

        assert_eq!(code, ExitCode::SUCCESS);
        let shown = ui.shown();
        assert_eq!(
            shown[..3],
            [
                "Stopped: the request to the model was interrupted",
                "Tool calls in this turn: write_file (1)",
                "Files written in this turn: src/main.rs",
            ]
        );
        assert_eq!(shown.last().map(String::as_str), Some("back again"));
        assert_eq!(ui.asked(), ["molt> ", "molt> ", "molt> "]);
        assert_eq!(
            fs::read_to_string(dir.path().join("src/main.rs")).unwrap(),
            PRINT_1
        );
    }
}
