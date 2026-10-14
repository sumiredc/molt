//! The [`Ui`] for a person at a terminal (PRD §8.1). Questions go through rustyline: lines with
//! history and editing, and choices and approvals as lines too, so that Ctrl-C means the same at
//! every question (`Input::Interrupted` or no answer) and never ends molt by itself. What is shown
//! goes through [`screen`], which keeps the status line and writes styles with crossterm, and the
//! form is drawn with crossterm ([`form`]).

mod form;
mod screen;

use std::io;

use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;

use self::screen::Screen;
use super::form::{Form, FormValues};
use super::{Input, Tone, Ui, UiError};

pub(crate) struct TerminalUi {
    editor: DefaultEditor,
    screen: Screen,
}

impl TerminalUi {
    pub(crate) fn new() -> Result<Self, UiError> {
        let editor = DefaultEditor::new().map_err(terminal_error)?;
        Ok(Self {
            editor,
            screen: Screen::new(),
        })
    }

    /// Reads one line for an answer, which is not kept in the history. `None` for Ctrl-C and
    /// Ctrl-D: the user did not answer.
    fn read_answer(&mut self, prompt: &str) -> Result<Option<String>, UiError> {
        answer(self.editor.readline(prompt))
    }
}

impl Ui for TerminalUi {
    fn read_line(&mut self, prompt: &str) -> Result<Input, UiError> {
        self.screen.status(None);
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
        self.screen.print(text);
    }

    fn show_lines(&mut self, lines: &[(Tone, String)]) {
        self.screen.print_lines(lines);
    }

    fn show_markdown(&mut self, text: &str, indent: &str) {
        if !self.screen.styled() {
            self.screen
                .print(&super::indented(&super::printable(text), indent));
            return;
        }
        for line in text.lines() {
            self.screen.markdown_line(line, indent);
        }
        self.screen.end_markdown();
    }

    fn status(&mut self, doing: Option<&str>) {
        self.screen.status(doing);
    }

    fn streams(&self) -> bool {
        self.screen.styled()
    }

    fn stream_line(&mut self, line: &str, indent: &str) {
        self.screen.markdown_line(line, indent);
    }

    fn end_stream(&mut self) {
        self.screen.end_markdown();
    }

    /// Lists `items` numbered from 1 and reads a number, asking again until it is one of them.
    fn select(&mut self, prompt: &str, items: &[&str]) -> Result<Option<usize>, UiError> {
        self.screen.status(None);
        self.screen.print(prompt);
        for (index, item) in items.iter().enumerate() {
            self.screen.print(&format!("  {}) {item}", index + 1));
        }
        loop {
            let Some(line) = self.read_answer("> ")? else {
                return Ok(None);
            };
            if let Some(index) = select_answer(&line, items.len()) {
                return Ok(Some(index));
            }
            self.screen
                .print(&format!("Type a number from 1 to {}.", items.len()));
        }
    }

    /// Reads `y`/`yes` or `n`/`no` after `prompt`, asking again until it is one of them.
    fn confirm(&mut self, prompt: &str) -> Result<Option<bool>, UiError> {
        // A prompt of several lines is printed, and only its last line stays beside the answer.
        let (head, last) = match prompt.rsplit_once('\n') {
            Some((head, last)) => (Some(head), last),
            None => (None, prompt),
        };
        self.screen.status(None);
        if let Some(head) = head {
            self.screen.print(head);
        }
        let question = if last.is_empty() {
            "[y/n] ".to_owned()
        } else {
            format!("{last} [y/n] ")
        };
        loop {
            let Some(line) = self.read_answer(&question)? else {
                return Ok(None);
            };
            if let Some(answer) = confirm_answer(&line) {
                return Ok(Some(answer));
            }
            self.screen.print("Type y or n.");
        }
    }

    fn form(
        &mut self,
        form: &Form,
        initial: Option<&FormValues>,
    ) -> Result<Option<FormValues>, UiError> {
        self.screen.status(None);
        form::fill_in(form, initial)
    }
}

/// The line read for an answer. Ctrl-C and Ctrl-D are no answer; any other failure is a terminal
/// error.
fn answer(read: Result<String, ReadlineError>) -> Result<Option<String>, UiError> {
    match read {
        Ok(line) => Ok(Some(line)),
        Err(ReadlineError::Interrupted | ReadlineError::Eof) => Ok(None),
        Err(error) => Err(terminal_error(error)),
    }
}

/// `y` or `yes` is true, `n` or `no` is false, in any case and with spaces around. Anything else
/// is no answer.
fn confirm_answer(line: &str) -> Option<bool> {
    match line.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => Some(true),
        "n" | "no" => Some(false),
        _ => None,
    }
}

/// The index of the item numbered `line` (from 1) among `items` items, if it is one.
fn select_answer(line: &str, items: usize) -> Option<usize> {
    let number: usize = line.trim().parse().ok()?;
    (1..=items).contains(&number).then(|| number - 1)
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
    fn ctrl_c_and_ctrl_d_at_a_confirmation_or_a_choice_are_no_answer() {
        assert_eq!(answer(Err(ReadlineError::Interrupted)).unwrap(), None);
        assert_eq!(answer(Err(ReadlineError::Eof)).unwrap(), None);
    }

    #[test]
    fn a_line_read_for_an_answer_is_the_answer() {
        assert_eq!(answer(Ok("y".to_owned())).unwrap().as_deref(), Some("y"));
    }

    #[test]
    fn other_failures_reading_an_answer_are_terminal_errors() {
        let failed = io::Error::new(io::ErrorKind::BrokenPipe, "closed");
        assert!(matches!(
            answer(Err(ReadlineError::Io(failed))),
            Err(UiError::Terminal(error)) if error.kind() == io::ErrorKind::BrokenPipe
        ));
    }

    #[test]
    fn yes_and_no_in_any_case_answer_a_confirmation() {
        for line in ["y", "Y", "yes", " YES "] {
            assert_eq!(confirm_answer(line), Some(true), "{line:?}");
        }
        for line in ["n", "N", "no", " No "] {
            assert_eq!(confirm_answer(line), Some(false), "{line:?}");
        }
    }

    #[test]
    fn anything_else_does_not_answer_a_confirmation() {
        for line in ["", "  ", "yep", "1", "ok"] {
            assert_eq!(confirm_answer(line), None, "{line:?}");
        }
    }

    #[test]
    fn a_number_of_an_item_chooses_it_counting_from_1() {
        assert_eq!(select_answer("1", 3), Some(0));
        assert_eq!(select_answer(" 3 ", 3), Some(2));
    }

    #[test]
    fn a_number_outside_the_items_or_not_a_number_chooses_nothing() {
        for line in ["0", "4", "-1", "", "two"] {
            assert_eq!(select_answer(line, 3), None, "{line:?}");
        }
    }
}
