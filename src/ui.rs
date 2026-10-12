//! The boundary between molt and its user (PRD §8.2): reading a line, showing text, choosing one
//! of several items and approving an operation.
//!
//! `TerminalUi` talks to a person through rustyline (lines with history and editing, and choices
//! and approvals as lines), and draws the [`Form`] of `molt init` with crossterm (PRD §8.1).
//! [`ScriptedUi`] answers from a script instead, so tests and `--ui-script` (PRD §8.6) run without
//! a terminal.

mod form;
mod scripted;
mod terminal;

use std::error::Error;
use std::fmt;
use std::io;

pub use form::{Form, FormText, FormValues, ITEMS, Tab, TabValues};
pub use scripted::{FormTabStep, RecordedForm, ScriptError, ScriptedUi, Step};
pub(crate) use terminal::TerminalUi;

/// What the user gave at a line prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// A line, without the line break.
    Line(String),
    /// Ctrl-C.
    Interrupted,
    /// Ctrl-D, or the end of the input.
    Eof,
}

/// Talks to the user. Fixed strings passed in and shown are English (PRD §8.6), except those of a
/// [`Form`], whose language the caller chooses (PRD §3).
pub trait Ui {
    /// Shows `prompt` and reads one line.
    fn read_line(&mut self, prompt: &str) -> Result<Input, UiError>;

    /// Shows `text` followed by a line break.
    fn show(&mut self, text: &str);

    /// Lets the user choose one of `items`. Returns its index, or `None` when the user
    /// interrupted instead of choosing.
    fn select(&mut self, prompt: &str, items: &[&str]) -> Result<Option<usize>, UiError>;

    /// Asks a yes/no question. Returns the answer, or `None` when the user interrupted instead
    /// of answering.
    fn confirm(&mut self, prompt: &str) -> Result<Option<bool>, UiError>;

    /// Lets the user fill in `form`, opened with `initial` (or empty), and submit it. Returns
    /// everything entered, or `None` when the user interrupted instead of submitting.
    fn form(
        &mut self,
        form: &Form,
        initial: Option<&FormValues>,
    ) -> Result<Option<FormValues>, UiError>;
}

/// Why the [`Ui`] could not get an answer.
#[derive(Debug)]
pub enum UiError {
    /// Reading from or writing to the terminal failed.
    Terminal(io::Error),
    /// The script has no step left for the question.
    ScriptExhausted { asked: &'static str },
    /// The next step in the script answers a different kind of question.
    ScriptMismatch { asked: &'static str, step: Step },
    /// The script chooses a number that is not one of the items (numbers start at 1).
    SelectOutOfRange { number: usize, items: usize },
    /// The script fills in a form with a different number of tabs.
    FormTabCount { tabs: usize, script: usize },
    /// The script chooses or checks a number that is not one in tab `tab` (numbers start at 1).
    FormOutOfRange {
        tab: usize,
        what: &'static str,
        number: usize,
        items: usize,
    },
    /// A form was asked for, but molt is not at a terminal.
    NotATerminal,
}

impl fmt::Display for UiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UiError::Terminal(error) => write!(f, "terminal error: {error}"),
            UiError::ScriptExhausted { asked } => {
                write!(f, "the ui script has no step left for {asked}")
            }
            UiError::ScriptMismatch { asked, step } => write!(
                f,
                "the ui script was asked for {asked}, but its next step is {}",
                step.kind()
            ),
            UiError::SelectOutOfRange { number, items } => write!(
                f,
                "the ui script selects {number}, but there are {items} items (numbered from 1)"
            ),
            UiError::FormTabCount { tabs, script } => write!(
                f,
                "the ui script fills in {script} tabs, but the form has {tabs}"
            ),
            UiError::FormOutOfRange {
                tab,
                what,
                number,
                items,
            } => write!(
                f,
                "the ui script gives {what} {number} in tab {tab}, but it has {items} (numbered from 1)"
            ),
            UiError::NotATerminal => {
                f.write_str("the form needs a terminal; give the answers with --ui-script instead")
            }
        }
    }
}

impl Error for UiError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            UiError::Terminal(error) => Some(error),
            _ => None,
        }
    }
}
