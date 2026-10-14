//! ③ on the side line on the terminal (PRD UC3): molt takes the step back, guides where to write
//! what, then `/done` checks what the user wrote and shows how it went.

use super::{HINT, Left, indent};
use crate::agent::mainline::MainLine;
use crate::agent::subline::{Done, Failure, SubLine};
use crate::cli::mainline::show_error_of;
use crate::llm::ChatError;
use crate::ui::Ui;

/// What the user can do while they write in ③.
pub(super) const WRITING: &str = "Write it in your editor, then type /done to have molt check it. \
     You can still ask questions, /choose another way, or /abort to withdraw the request.";

/// Starts ③ on `side`, the side line of `agent`: takes the step back, says so, then shows where to
/// write what, as the model tells it, and what to do next. [`Left::Interrupted`] when the user
/// interrupted the wait for the guidance.
pub(super) fn guide(ui: &mut dyn Ui, side: &mut SubLine, agent: &mut MainLine) -> Option<Left> {
    if let Some(taken) = agent.take_back() {
        ui.show(&indent(&super::super::taken_back_text(&taken)));
    }
    side.start_writing(agent.tools());
    let log = agent.log();
    match side.guide(log) {
        Ok(guidance) => ui.show(&indent(&guidance)),
        Err(ChatError::Interrupted) => {
            show_error_of(ui, log);
            return Some(Left::Interrupted);
        }
        Err(error) => ui.show(&indent(&format!(
            "Could not get the guidance on where to write it: {error}"
        ))),
    }
    show_error_of(ui, log);
    ui.show(&indent(WRITING));
    None
}

/// `/done` on `side`, the side line of `agent`: shows what came of it, and returns how to leave
/// the side line: with the outcome for the main line when it passed, or [`Left::Interrupted`]
/// when the user interrupted the wait for the review. Before ③ is chosen, only says to choose it
/// first.
pub(super) fn done(ui: &mut dyn Ui, side: &mut SubLine, agent: &MainLine) -> Option<Left> {
    if !side.is_writing() {
        ui.show(&indent(
            "Type /choose and choose to write it yourself first, then /done when you wrote it.",
        ));
        return None;
    }
    let done = agent.done(side);
    let shown = match done {
        Done::NoChange => {
            ui.show(&indent(&format!(
                "No change: no .rs file or Cargo.toml was written since molt took the step back. {HINT}"
            )));
            None
        }
        Done::Failed(failure) => {
            ui.show(&indent(&failed(&failure)));
            ui.show(&indent(
                "Your profile is unchanged. Fix it and type /done again, or /choose another way.",
            ));
            None
        }
        Done::Passed {
            resolution,
            review,
            check,
        } => {
            match &check {
                Ok(output) if !output.success => ui.show(&indent(&format!(
                    "cargo check found no error in your lines. Elsewhere:\n{}",
                    printed(output)
                ))),
                _ => ui.show(&indent("cargo check passed.")),
            }
            match review {
                Ok(review) => ui.show(&indent(&review)),
                Err(error) => ui.show(&indent(&format!("Could not get the review: {error}"))),
            }
            Some(Left::Wrote(resolution))
        }
        // Nothing changed, so the main line is paused at the boundary as after Ctrl-C.
        Done::Interrupted => Some(Left::Interrupted),
    };
    show_error_of(ui, agent.log());
    shown
}

/// Why `/done` did not pass: where the user's lines do not parse, and the `cargo check`.
fn failed(failure: &Failure) -> String {
    let mut lines: Vec<String> = failure
        .parse_errors
        .iter()
        .map(|(path, error)| format!("{path}:{error}"))
        .collect();
    match &failure.check {
        Ok(output) if output.success => {}
        // Only the user's lines did not parse: the errors elsewhere do not stop `/done`.
        Ok(output) if !failure.parse_errors.is_empty() => lines.push(format!(
            "cargo check failed, though not only in your lines:\n{}",
            printed(output)
        )),
        Ok(output) => lines.push(format!(
            "cargo check found errors in your lines:\n{}",
            printed(output)
        )),
        Err(error) => lines.push(format!("cargo check could not run: {error}")),
    }
    lines.join("\n")
}

/// The output of `cargo check`, stdout then stderr.
fn printed(output: &crate::workspace::cargo::CargoOutput) -> String {
    [output.stdout.trim_end(), output.stderr.trim_end()]
        .into_iter()
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// ③ on the side line through `/choose` and `/done`, and what the main line does with it. Named so
/// that `cargo test subline::user_wrote` selects them.
#[cfg(test)]
mod tests;
