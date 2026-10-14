//! Asking the user for approval of the tools' operations through a [`Ui`] (PRD §8.4).

use crate::agent::mainline::Resolved;
use crate::agent::tools::{ApprovalRequest, Approver};
use crate::ui::Ui;
use crate::ui::markdown::Folder;

/// Asks for approval through a [`Ui`], so the tools can ask the user (PRD §8.4), and shows what
/// the main line is doing while the user waits (PRD §8.1).
pub struct UiApprover<'a> {
    ui: &'a mut dyn Ui,
    /// Folds the code blocks of the model's text.
    folder: Folder,
    /// The text of the response so far, when the [`Ui`] shows it whole.
    held: Vec<String>,
}

impl<'a> UiApprover<'a> {
    pub fn new(ui: &'a mut dyn Ui) -> Self {
        Self {
            ui,
            folder: Folder::default(),
            held: Vec::new(),
        }
    }

    fn put(&mut self, line: String) {
        if self.ui.streams() {
            self.ui.stream_line(&line, "");
        } else {
            self.held.push(line);
        }
    }
}

impl Approver for UiApprover<'_> {
    /// Only an explicit yes approves. An interruption or a failure to ask is not approval.
    fn confirm(&mut self, request: &ApprovalRequest) -> bool {
        let answer = match request {
            ApprovalRequest::CargoAdd(name) => self
                .ui
                .confirm(&format!(
                    "Add the crate `{}` with cargo add?",
                    name.as_str()
                ))
                .map(|answer| answer == Some(true)),
            ApprovalRequest::GitCommit(proposal) => super::commit::confirm(self.ui, proposal),
        };
        match answer {
            Ok(approved) => approved,
            Err(error) => {
                self.ui
                    .show(&format!("could not ask for approval: {error}"));
                false
            }
        }
    }

    fn working(&mut self, doing: Option<&str>) {
        self.ui.status(doing);
    }

    fn text(&mut self, line: &str) {
        if let Some(line) = self.folder.line(line) {
            self.put(line);
        }
    }

    fn text_end(&mut self) {
        if let Some(line) = self.folder.end() {
            self.put(line);
        }
        if self.ui.streams() {
            self.ui.end_stream();
            return;
        }
        let text = std::mem::take(&mut self.held).join("\n");
        if !text.trim().is_empty() {
            self.ui.show_markdown(&text, "");
        }
    }

    fn tool_ran(&mut self, line: &str) {
        self.ui.show(line);
    }

    fn resolved(&mut self, resolved: &Resolved<'_>) {
        for line in super::resolved_text(resolved) {
            self.ui.show(&line);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::{ScriptedUi, Step};
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
    fn the_models_text_is_shown_once_complete_with_its_code_blocks_folded() {
        let mut ui = ScriptedUi::new([]);
        let mut approver = UiApprover::new(&mut ui);
        for line in [
            "I will add this:",
            "```rust",
            "fn main() {}",
            "```",
            "Done.",
        ] {
            approver.text(line);
        }
        approver.text_end();
        approver.text_end();
        assert_eq!(ui.shown(), ["I will add this:\n[code: 1 line]\nDone."]);
    }

    #[test]
    fn where_text_streams_each_line_is_shown_as_it_comes_with_code_folded() {
        let mut ui = ScriptedUi::new([]).streaming();
        let mut approver = UiApprover::new(&mut ui);
        approver.working(Some("Thinking…"));
        for line in [
            "I will add this:",
            "```rust",
            "fn main() {}",
            "fn f() {}",
            "```",
        ] {
            approver.text(line);
        }
        approver.text_end();
        approver.working(None);
        assert_eq!(ui.shown(), ["I will add this:", "[code: 2 lines]"]);
        assert_eq!(ui.statuses(), [Some("Thinking…".to_owned()), None]);
    }

    #[test]
    fn a_code_block_not_closed_is_folded_to_the_end_of_the_text() {
        let mut ui = ScriptedUi::new([]);
        let mut approver = UiApprover::new(&mut ui);
        for line in ["Here:", "```", "a", "b"] {
            approver.text(line);
        }
        approver.text_end();
        assert_eq!(ui.shown(), ["Here:\n[code: 2 lines]"]);
    }

    #[test]
    fn a_tool_line_is_shown_as_it_is() {
        let mut ui = ScriptedUi::new([]);
        UiApprover::new(&mut ui).tool_ran("● read_file src/main.rs");
        assert_eq!(ui.shown(), ["● read_file src/main.rs"]);
    }

    #[test]
    fn approver_asks_with_the_crate_name() {
        let mut ui = ScriptedUi::new([Step::Confirm(true)]);
        UiApprover::new(&mut ui).confirm(&cargo_add());
        let asked = ui.asked();
        assert!(asked[0].contains("`serde`"), "{asked:?}");
    }
}
