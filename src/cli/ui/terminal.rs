//! The [`Ui`] for a person at a terminal: rustyline for lines (history and editing), dialoguer for
//! choices and approvals (PRD §8.1).

use std::io;

use dialoguer::{Confirm, Select};
use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;

use super::{Input, Ui, UiError};

pub(crate) struct TerminalUi {
    editor: DefaultEditor,
}

impl TerminalUi {
    pub(crate) fn new() -> Result<Self, UiError> {
        let editor = DefaultEditor::new().map_err(terminal_error)?;
        Ok(Self { editor })
    }
}

impl Ui for TerminalUi {
    fn read_line(&mut self, prompt: &str) -> Result<Input, UiError> {
        match self.editor.readline(prompt) {
            Ok(line) => {
                if !line.trim().is_empty() {
                    // The history is only a convenience; failing to keep a line is not an error.
                    let _ = self.editor.add_history_entry(line.as_str());
                }
                Ok(Input::Line(line))
            }
            Err(ReadlineError::Interrupted) => Ok(Input::Interrupted),
            Err(ReadlineError::Eof) => Ok(Input::Eof),
            Err(error) => Err(terminal_error(error)),
        }
    }

    fn show(&mut self, text: &str) {
        println!("{text}");
    }

    fn select(&mut self, prompt: &str, items: &[&str]) -> Result<Option<usize>, UiError> {
        let answer = Select::new()
            .with_prompt(prompt)
            .items(items)
            .default(0)
            .interact_opt();
        dialog_answer(answer)
    }

    fn confirm(&mut self, prompt: &str) -> Result<Option<bool>, UiError> {
        let answer = Confirm::new().with_prompt(prompt).interact_opt();
        dialog_answer(answer)
    }
}

/// Esc (`Ok(None)`) and Ctrl-C both mean the user did not answer.
fn dialog_answer<T>(answer: Result<Option<T>, dialoguer::Error>) -> Result<Option<T>, UiError> {
    match answer {
        Ok(answer) => Ok(answer),
        Err(dialoguer::Error::IO(error)) if error.kind() == io::ErrorKind::Interrupted => Ok(None),
        Err(dialoguer::Error::IO(error)) => Err(UiError::Terminal(error)),
    }
}

fn terminal_error(error: ReadlineError) -> UiError {
    match error {
        ReadlineError::Io(error) => UiError::Terminal(error),
        error => UiError::Terminal(io::Error::other(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn esc_in_a_dialog_is_no_answer() {
        assert_eq!(dialog_answer::<bool>(Ok(None)).unwrap(), None);
    }

    #[test]
    fn ctrl_c_in_a_dialog_is_no_answer() {
        let interrupted = io::Error::new(io::ErrorKind::Interrupted, "read interrupted");
        assert_eq!(
            dialog_answer::<bool>(Err(dialoguer::Error::IO(interrupted))).unwrap(),
            None
        );
    }

    #[test]
    fn other_dialog_failures_are_terminal_errors() {
        let failed = io::Error::new(io::ErrorKind::BrokenPipe, "closed");
        assert!(matches!(
            dialog_answer::<bool>(Err(dialoguer::Error::IO(failed))),
            Err(UiError::Terminal(error)) if error.kind() == io::ErrorKind::BrokenPipe
        ));
    }
}
