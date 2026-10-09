//! `molt` without a subcommand: checks UC2's preconditions, then opens the main line in the cargo
//! project at cwd (PRD §8.6).

use std::path::Path;
use std::process::ExitCode;

use super::profile;
use super::ui::{Input, ScriptedUi, TerminalUi, Ui};

/// The main-line prompt (PRD §8.6).
const PROMPT: &str = "molt> ";

/// The exit code after Ctrl-C at the prompt (128 + SIGINT, as a shell reports it).
const INTERRUPTED: u8 = 130;

pub(super) fn run(profile_flag: Option<&Path>, ui_script: Option<&Path>) -> ExitCode {
    match preconditions(profile_flag).and_then(|()| open_ui(ui_script)) {
        Ok(mut ui) => main_line(ui.as_mut()),
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

/// UC2's preconditions: a profile exists, and the current directory is a cargo project.
fn preconditions(profile_flag: Option<&Path>) -> Result<(), String> {
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
    Ok(())
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

/// Reads requests until `/quit` or Ctrl-D (exit 0) or Ctrl-C (exit non-zero). Requests are taken
/// and nothing is done with them yet.
fn main_line(ui: &mut dyn Ui) -> ExitCode {
    loop {
        match ui.read_line(PROMPT) {
            Ok(Input::Line(line)) if line.trim() == "/quit" => return ExitCode::SUCCESS,
            Ok(Input::Line(_)) => {}
            Ok(Input::Eof) => return ExitCode::SUCCESS,
            Ok(Input::Interrupted) => return ExitCode::from(INTERRUPTED),
            Err(error) => {
                eprintln!("{error}");
                return ExitCode::FAILURE;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::ui::Step;

    fn run_script(steps: impl IntoIterator<Item = Step>) -> (ExitCode, ScriptedUi) {
        let mut ui = ScriptedUi::new(steps);
        let code = main_line(&mut ui);
        (code, ui)
    }

    fn input(line: &str) -> Step {
        Step::Input(line.to_owned())
    }

    #[test]
    fn quit_ends_the_main_line_with_success() {
        let (code, _) = run_script([input("/quit")]);
        assert_eq!(code, ExitCode::SUCCESS);
    }

    #[test]
    fn quit_with_surrounding_spaces_still_ends_the_main_line() {
        let (code, ui) = run_script([input("  /quit "), input("/quit")]);
        assert_eq!(code, ExitCode::SUCCESS);
        assert_eq!(
            ui.asked().len(),
            1,
            "the main line ended at the first /quit"
        );
    }

    #[test]
    fn eof_ends_the_main_line_with_success() {
        let (code, _) = run_script([Step::Eof]);
        assert_eq!(code, ExitCode::SUCCESS);
    }

    #[test]
    fn interrupt_ends_the_main_line_with_a_non_zero_code() {
        let (code, _) = run_script([Step::Interrupt]);
        assert_eq!(code, ExitCode::from(INTERRUPTED));
    }

    #[test]
    fn other_inputs_are_taken_and_the_prompt_comes_back() {
        let (code, ui) = run_script([input("write a parser"), input(""), input("/quit")]);
        assert_eq!(code, ExitCode::SUCCESS);
        assert_eq!(ui.asked(), ["molt> "; 3]);
        assert!(ui.shown().is_empty());
    }

    #[test]
    fn a_ui_error_ends_the_main_line_with_failure() {
        let (code, _) = run_script([]);
        assert_eq!(code, ExitCode::FAILURE);
    }
}
