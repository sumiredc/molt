//! How a commit of the files molt changed shows on the main line (PRD UC4): the diff and the
//! message, the warning and the other changes, the approval, and what came of it.

use super::diff;
use crate::agent::mainline::{CommitOutcome, CommitProposal, MainLine, Turn};
use crate::cli::ui::{Ui, UiApprover, UiError};

/// Shows `proposal` and asks the user to approve it: first whether to commit despite the failing
/// check, then whether to leave the other changes out, then the commit itself. A no or an
/// interruption at any of them is no.
pub(in crate::cli) fn confirm(ui: &mut dyn Ui, proposal: &CommitProposal) -> Result<bool, UiError> {
    ui.show("To commit:");
    if !proposal.diff.trim().is_empty() {
        ui.show(proposal.diff.trim_end());
    }
    for (path, content) in &proposal.new_files {
        let diff = diff::unified(path, None, content);
        if !diff.is_empty() {
            ui.show(&diff);
        }
    }
    ui.show(&format!("Message:\n{}", indented(&proposal.message)));
    if proposal.check_failing {
        ui.show("Warning: the last cargo check molt ran did not pass.");
        if ui.confirm("Commit anyway?")? != Some(true) {
            return Ok(false);
        }
    }
    if !proposal.others.is_empty() {
        ui.show(&format!(
            "Other changes are not part of this commit and stay as they are:\n{}",
            indented(&proposal.others.join("\n"))
        ));
        if ui.confirm("Stage and commit only the files molt changed?")? != Some(true) {
            return Ok(false);
        }
    }
    Ok(ui.confirm("Commit with this message?")? == Some(true))
}

/// Proposes a commit after `turn` when molt has one to propose, and shows what came of it.
pub(super) fn offer(ui: &mut dyn Ui, agent: &mut MainLine, turn: &Turn) {
    if let Some(outcome) = agent.offer_commit(turn, &mut UiApprover::new(ui)) {
        show_outcome(ui, &outcome);
    }
}

/// What came of a commit.
pub(super) fn show_outcome(ui: &mut dyn Ui, outcome: &CommitOutcome) {
    let text = match outcome {
        CommitOutcome::Committed { output, .. } => {
            format!("Committed.\n{}", output.trim_end())
        }
        CommitOutcome::NotApproved => "Not committed.".to_owned(),
        CommitOutcome::NothingToCommit => {
            "Nothing to commit: the files molt changed are as git has them.".to_owned()
        }
        CommitOutcome::NotARepository(error) => {
            format!("Not committed: this is not a git repository.\n{error}")
        }
        CommitOutcome::Failed {
            staged: true,
            error,
        } => {
            format!("The commit failed, so nothing was committed. The files stay staged.\n{error}")
        }
        CommitOutcome::Failed {
            staged: false,
            error,
        } => {
            format!("The commit failed, so nothing was committed.\n{error}")
        }
    };
    ui.show(text.trim_end());
}

/// `text` with each line indented by 2 spaces.
fn indented(text: &str) -> String {
    text.lines()
        .map(|line| format!("  {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Tests of a commit on the main line. The module path has `mainline::commit`, so that
/// `cargo test commit` selects them.
#[cfg(test)]
mod tests;
