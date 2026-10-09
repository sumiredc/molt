//! The end of a turn that wrote (PRD §3 Verification, §8.4, UC2): molt adds the `known` crates
//! the code needs, with the user's approval, and runs `cargo check` whether or not the model ran
//! it. When the model is done and the check fails, the errors go back to the model for a compile
//! fix, up to [`COMPILE_FIX_BUDGET`] in a turn.

use serde_json::json;

use super::{
    AddOutcome, COMPILE_FIX_BUDGET, Change, CrateAdd, MANIFEST, MainLine, Progress, Turn, TurnEnd,
    Withdrawal,
};
use crate::agent::prompts;
use crate::agent::tools::{Approver, ToolError};
use crate::llm::{Message, ToolCall};
use crate::workspace::cargo::CrateName;

impl MainLine<'_> {
    /// Ends the model's part of the turn, which stopped at `end`. Returns how the turn ends, or
    /// `None` when the model is to fix what `cargo check` found, in the same turn.
    ///
    /// At a boundary, nothing is run: the turn is held for the user's choice there (UC3), not
    /// done. Otherwise, if anything was written since the last check, molt adds the crates and
    /// runs `cargo check`.
    pub(super) fn end_turn(
        &mut self,
        end: TurnEnd,
        turn: &mut Turn,
        progress: &mut Progress,
        approver: &mut dyn Approver,
    ) -> Option<TurnEnd> {
        if matches!(end, TurnEnd::Boundary(_)) || !progress.unchecked {
            return Some(end);
        }
        progress.unchecked = false;
        // Crates are added only for the model to go on. After a stop, the state is only shown.
        let notes = if matches!(end, TurnEnd::Finished) {
            self.add_crates(turn, progress, approver)
        } else {
            Vec::new()
        };
        self.update_changes(&mut turn.changes, progress.manifest_before.as_deref());
        let check = self.tools.cargo_check();
        let failed = match &check {
            Ok(output) if !output.success => Some(prompts::check_failed(
                &output.stdout,
                &output.stderr,
                &notes,
            )),
            _ => None,
        };
        turn.check = Some(check);
        let result = match failed {
            Some(result) if matches!(end, TurnEnd::Finished) => result,
            _ => return Some(end),
        };
        if turn.compile_fixes == COMPILE_FIX_BUDGET {
            return Some(TurnEnd::CompileFixesExhausted);
        }
        turn.compile_fixes += 1;
        // What the model said before the check is not its last word any more.
        turn.reply = None;
        // The failure goes back as the result of a `cargo check` call in the model's name, not as
        // a user message, so that the user's input stays the latest one and the model keeps
        // answering in its language (PRD §3 Language).
        self.molt_checks += 1;
        let id = format!("molt_check_{}", self.molt_checks);
        let check_call = ToolCall {
            id: id.clone(),
            name: "run_cargo".to_owned(),
            arguments: json!({ "subcommand": "check" }).to_string(),
        };
        // After a reply without tool calls, the call joins that reply, since some chat templates
        // refuse two assistant messages in a row.
        match self.messages.last_mut() {
            Some(Message::Assistant { tool_calls, .. }) if tool_calls.is_empty() => {
                tool_calls.push(check_call);
            }
            _ => self.messages.push(Message::Assistant {
                content: None,
                tool_calls: vec![check_call],
            }),
        }
        self.messages.push(Message::Tool {
            tool_call_id: id,
            content: result,
        });
        None
    }

    /// Withdraws the request of the turn held at a boundary (UC3's `/abort`): nothing more is
    /// changed and nothing goes to the model, but what was written before the boundary is
    /// checked with `cargo check` and handed back to be shown (PRD §3 Verification). Empty when
    /// no turn is held, or the held turn changed nothing.
    pub fn withdraw(&mut self) -> Withdrawal {
        let Some(mut held) = self.held.take() else {
            return Withdrawal {
                changes: Vec::new(),
                check: None,
            };
        };
        if held.changes.is_empty() && !held.unchecked {
            return Withdrawal {
                changes: Vec::new(),
                check: None,
            };
        }
        self.update_changes(&mut held.changes, held.manifest_before.as_deref());
        Withdrawal {
            changes: held.changes,
            check: Some(self.tools.cargo_check()),
        }
    }

    /// Asks the user to `cargo add` each crate the files written in this turn use that the
    /// profile allows but `Cargo.toml` does not declare (UC2), unless it was already asked about
    /// in this turn and not added: the user said no, or `cargo add` failed. Returns notes for the
    /// model on the crates that were not added.
    fn add_crates(
        &self,
        turn: &mut Turn,
        progress: &mut Progress,
        approver: &mut dyn Approver,
    ) -> Vec<String> {
        let mut notes = Vec::new();
        for name in self
            .tools
            .undeclared_known_crates(&turn.written, &self.profile)
        {
            let key = name.replace('-', "_");
            if let Some(note) = progress.not_added.get(&key) {
                notes.push(note.clone());
                continue;
            }
            let outcome = match CrateName::new(&name) {
                Err(error) => AddOutcome::Failed(error.to_string()),
                Ok(crate_name) => match self.tools.add_crate(&crate_name, approver) {
                    Ok(output) => AddOutcome::Ran(output),
                    Err(ToolError::NotApproved) => AddOutcome::NotApproved,
                    Err(error) => AddOutcome::Failed(error.to_string()),
                },
            };
            let note = match &outcome {
                AddOutcome::Ran(output) if output.success => None,
                AddOutcome::Ran(output) => {
                    Some(prompts::crate_not_added(&name, output.stderr.trim()))
                }
                AddOutcome::NotApproved => Some(prompts::crate_not_approved(&name)),
                AddOutcome::Failed(why) => Some(prompts::crate_not_added(&name, why)),
            };
            if let Some(note) = note {
                notes.push(note.clone());
                progress.not_added.insert(key, note);
            }
            turn.crate_adds.push(CrateAdd { name, outcome });
        }
        notes
    }

    /// Reads the content of each changed file back from the disk, where `run_cargo fmt` may have
    /// changed it since it was written, and puts the `Cargo.toml` at the root among the changes
    /// when it is no longer what it was when the turn started, as after `cargo add`.
    fn update_changes(&self, changes: &mut Vec<Change>, manifest_before: Option<&str>) {
        for change in changes.iter_mut() {
            if let Some(now) = self.tools.read(&change.path) {
                change.after = now;
            }
        }
        let Some(now) = self.tools.read(MANIFEST) else {
            return;
        };
        let tools = &self.tools;
        match changes
            .iter_mut()
            .find(|c| tools.same_file(&c.path, MANIFEST))
        {
            Some(change) => change.after = now,
            None if manifest_before != Some(now.as_str()) => {
                changes.push(Change {
                    path: MANIFEST.to_owned(),
                    before: manifest_before.map(str::to_owned),
                    after: now,
                });
            }
            None => {}
        }
    }
}

/// Tests of the end of a turn. The module path has `mainline::turn_end`, so that
/// `cargo test mainline::turn_end` selects them.
#[cfg(test)]
mod tests;
