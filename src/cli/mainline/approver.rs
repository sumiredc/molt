//! Asking the user for approval of the tools' operations through a [`Ui`] (PRD §8.4).

use crate::agent::tools::{ApprovalRequest, Approver};
use crate::ui::Ui;

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
    fn approver_asks_with_the_crate_name() {
        let mut ui = ScriptedUi::new([Step::Confirm(true)]);
        UiApprover::new(&mut ui).confirm(&cargo_add());
        let asked = ui.asked();
        assert!(asked[0].contains("`serde`"), "{asked:?}");
    }
}
