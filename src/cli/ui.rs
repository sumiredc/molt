//! The boundary between molt and its user (PRD §8.2): reading a line, showing text, choosing one
//! of several items and approving an operation.
//!
//! `TerminalUi` talks to a person through rustyline (lines, with history and editing) and
//! dialoguer (choices and approvals) (PRD §8.1). [`ScriptedUi`] answers from a script instead, so
//! tests and `--ui-script` (PRD §8.6) run without a terminal.

mod scripted;
mod terminal;

use std::error::Error;
use std::fmt;
use std::io;

use crate::agent::tools::{ApprovalRequest, Approver};

pub use scripted::{ScriptError, ScriptedUi, Step};
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

/// Talks to the user. Fixed strings passed in and shown are English (PRD §8.6).
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

/// Asks for approval through a [`Ui`], so the tools can ask the user (PRD §8.4).
pub struct UiApprover<'a> {
    ui: &'a mut dyn Ui,
}

impl<'a> UiApprover<'a> {
    pub fn new(ui: &'a mut dyn Ui) -> Self {
        Self { ui }
    }
}

impl Approver for UiApprover<'_> {
    /// Only an explicit yes approves. An interruption or a failure to ask is not approval.
    fn confirm(&mut self, request: &ApprovalRequest) -> bool {
        let prompt = match request {
            ApprovalRequest::CargoAdd(name) => {
                format!("Add the crate `{}` with cargo add?", name.as_str())
            }
            ApprovalRequest::GitCommit { message } => {
                format!("Commit the staged changes with this message?\n\n{message}\n")
            }
        };
        match self.ui.confirm(&prompt) {
            Ok(answer) => answer == Some(true),
            Err(error) => {
                self.ui
                    .show(&format!("could not ask for approval: {error}"));
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::cargo::CrateName;

    fn cargo_add() -> ApprovalRequest {
        ApprovalRequest::CargoAdd(CrateName::new("serde").unwrap())
    }

    #[test]
    fn approver_approves_when_the_ui_answers_yes() {
        let mut ui = ScriptedUi::new([Step::Confirm(true)]);
        assert!(UiApprover::new(&mut ui).confirm(&cargo_add()));
    }

    #[test]
    fn approver_does_not_approve_when_the_ui_answers_no() {
        let mut ui = ScriptedUi::new([Step::Confirm(false)]);
        assert!(!UiApprover::new(&mut ui).confirm(&cargo_add()));
    }

    #[test]
    fn approver_does_not_approve_when_the_user_interrupts() {
        let mut ui = ScriptedUi::new([Step::Interrupt]);
        assert!(!UiApprover::new(&mut ui).confirm(&cargo_add()));
    }

    #[test]
    fn approver_does_not_approve_and_shows_why_when_the_ui_cannot_ask() {
        let mut ui = ScriptedUi::new([]);
        assert!(!UiApprover::new(&mut ui).confirm(&cargo_add()));
        assert_eq!(
            ui.shown(),
            ["could not ask for approval: the ui script has no step left for a confirmation"]
        );
    }

    #[test]
    fn approver_asks_with_the_crate_name_and_the_commit_message() {
        let mut ui = ScriptedUi::new([Step::Confirm(true), Step::Confirm(true)]);
        let mut approver = UiApprover::new(&mut ui);
        approver.confirm(&cargo_add());
        approver.confirm(&ApprovalRequest::GitCommit {
            message: "Add a parser".to_owned(),
        });
        let asked = ui.asked();
        assert!(asked[0].contains("`serde`"), "{asked:?}");
        assert!(asked[1].contains("Add a parser"), "{asked:?}");
    }
}
