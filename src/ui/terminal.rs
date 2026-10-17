//! The [`Ui`] for a person at a terminal (PRD §8.1). Lines go through rustyline, with history and
//! editing, and so do approvals, so that Ctrl-C means the same at every question
//! (`Input::Interrupted` or no answer) and never ends molt by itself. The choices at a boundary
//! read keys with crossterm and hand a line to rustyline ([`choices`]). What is shown goes through
//! [`screen`], which keeps the status line and writes styles with crossterm, and the form is drawn
//! with crossterm ([`form`]).

mod choices;
mod form;
mod screen;

use std::io::{self, IsTerminal};

use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;

use self::choices::Keys;
use self::screen::Screen;
use super::form::{Form, FormValues};
use super::{Input, Picked, Tone, Ui, UiError};

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

    /// The choices listed, and a line read at `line_prompt`: the number of a choice listed
    /// chooses it, and any other line is the line.
    fn choose_by_line(
        &mut self,
        prompt: &str,
        choices: &[(usize, &str)],
        line_prompt: &str,
    ) -> Result<Picked, UiError> {
        self.screen.print(&super::printable(prompt));
        for (at, (number, label)) in choices.iter().enumerate() {
            self.screen.print(&choices::line(*number, label, at == 0));
        }
        let read = self.editor.readline(line_prompt);
        Ok(match self.picked_line(read)? {
            Picked::Line(line) => match picked_number(&line, choices) {
                Some(number) => Picked::Choice(number),
                None => Picked::Line(line),
            },
            picked => picked,
        })
    }

    /// The line read at the choices, kept in the history like one read at a prompt.
    fn picked_line(&mut self, read: Result<String, ReadlineError>) -> Result<Picked, UiError> {
        match read {
            Ok(line) => {
                if !line.trim().is_empty() {
                    let _ = self.editor.add_history_entry(line.as_str());
                }
                Ok(Picked::Line(line))
            }
            Err(ReadlineError::Interrupted) => Ok(Picked::Interrupted),
            Err(ReadlineError::Eof) => Ok(Picked::Eof),
            Err(error) => Err(terminal_error(error)),
        }
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
        self.screen.print(&super::printable(text));
    }

    fn show_lines(&mut self, lines: &[(Tone, String)]) {
        let lines: Vec<(Tone, String)> = lines
            .iter()
            .map(|(tone, line)| (*tone, super::printable(line).into_owned()))
            .collect();
        self.screen.print_lines(&lines);
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

    /// Keys choose when stdin and stdout are a terminal; otherwise a line chooses by number
    /// (PRD §8.6).
    fn choose(
        &mut self,
        prompt: &str,
        choices: &[(usize, &str)],
        line_prompt: &str,
    ) -> Result<Picked, UiError> {
        // The line prompt may name crates the model wrote.
        let line_prompt = &*super::printable(line_prompt);
        self.screen.status(None);
        if !io::stdin().is_terminal() || !self.screen.styled() {
            return self.choose_by_line(prompt, choices, line_prompt);
        }
        let started = match choices::read(prompt, choices, self.screen.styled())
            .map_err(UiError::Terminal)?
        {
            Keys::Picked(picked) => return Ok(picked),
            Keys::Typed(line) => {
                self.screen.print(&format!("{line_prompt}{line}"));
                return self.picked_line(Ok(line));
            }
            Keys::Line(started) => started,
        };
        let read = self
            .editor
            .readline_with_initial(line_prompt, (started.as_str(), ""));
        self.picked_line(read)
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
            self.screen.print(&super::printable(head));
        }
        let question = if last.is_empty() {
            "[y/n] ".to_owned()
        } else {
            format!("{} [y/n] ", super::printable(last))
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

/// The number of a choice listed that `line` is, as typed or full width.
fn picked_number(line: &str, choices: &[(usize, &str)]) -> Option<usize> {
    let mut chars = line.trim().chars();
    let number = chars.next().and_then(choices::number)?;
    (chars.next().is_none() && choices.iter().any(|(listed, _)| *listed == number))
        .then_some(number)
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
    fn ctrl_c_and_ctrl_d_at_a_confirmation_are_no_answer() {
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

    const CHOICES: [(usize, &str); 3] = [(1, "a"), (2, "b"), (4, "d")];

    #[test]
    fn with_stdin_not_a_terminal_a_line_with_the_number_of_a_choice_listed_chooses_it() {
        assert_eq!(picked_number("1", &CHOICES), Some(1));
        assert_eq!(picked_number(" 4 ", &CHOICES), Some(4));
        assert_eq!(picked_number("２", &CHOICES), Some(2));
    }

    #[test]
    fn with_stdin_not_a_terminal_any_other_line_is_a_line() {
        for line in ["3", "0", "5", "12", "", "two", "1?"] {
            assert_eq!(picked_number(line, &CHOICES), None, "{line:?}");
        }
    }
}
