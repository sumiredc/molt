//! The write path of the main line (PRD §8.4): `write_file` and `edit_file` reach the disk only
//! with content the check finds Clean. A rejection and a parse error go back to the model as the
//! tool result, and never to the user. The second rejection of the same item in a turn, or
//! `request_boundary`, stops the turn at a boundary.

use serde::Deserialize;
use serde_json::{Map, Value};

use super::{
    Boundary, BoundaryCause, COMPILE_FIX_BUDGET, Change, MainLine, Next, Progress, Turn, TurnEnd,
};
use crate::agent::prompts;
use crate::agent::tools::{EditFileArgs, FileChange, ToolError, WriteFileArgs, WriteOutcome};
use crate::profile::status::{ItemKey, Status};
use crate::profile::store::Profile;
use crate::session::{Detected, Event};

impl MainLine<'_> {
    /// Runs `write_file` or `edit_file` (`name`) through the check.
    pub(super) fn write(
        &self,
        name: &str,
        arguments: Map<String, Value>,
        turn: &mut Turn,
        progress: &mut Progress,
    ) -> (String, Next) {
        let change = match self.prepare(name, arguments) {
            Ok(change) => change,
            Err(error) => return (prompts::tool_error(error), Next::Continue),
        };
        let path = change.path().to_owned();
        let (before, after) = (
            change.old_content().map(str::to_owned),
            change.new_content().to_owned(),
        );
        let outcome = match self.tools.write(change, &self.profile) {
            Ok(outcome) => outcome,
            Err(error) => return (prompts::tool_error(error), Next::Continue),
        };
        match outcome {
            WriteOutcome::Written => {
                if !turn.written.contains(&path) {
                    turn.written.push(path.clone());
                }
                let tools = &self.tools;
                match turn
                    .changes
                    .iter_mut()
                    .find(|c| tools.same_file(&c.path, &path))
                {
                    Some(change) => change.after = after,
                    None => turn.changes.push(Change {
                        path: path.clone(),
                        before,
                        after,
                    }),
                }
                progress.unchecked = true;
                (prompts::written(&path), Next::Continue)
            }
            WriteOutcome::OutOfProfile { findings, change } => {
                self.log.record(&Event::Detected {
                    path: path.clone(),
                    items: findings
                        .iter()
                        .map(|finding| Detected {
                            line: finding.line,
                            item: finding.key.to_string(),
                            category: finding.category().map(|c| c.name().to_owned()),
                            text: finding.text.clone(),
                        })
                        .collect(),
                });
                let mut items: Vec<ItemKey> = Vec::new();
                for finding in &findings {
                    if !items.contains(&finding.key) {
                        items.push(finding.key.clone());
                    }
                }
                let repeated: Vec<ItemKey> = items
                    .iter()
                    .filter(|item| progress.rejected.contains(item))
                    .cloned()
                    .collect();
                self.log.record(&Event::Rejected {
                    path: path.clone(),
                    items: items.iter().map(ToString::to_string).collect(),
                    content: change.new_content().to_owned(),
                    boundary: !repeated.is_empty(),
                });
                if repeated.is_empty() {
                    progress.rejected_now.extend(items);
                    return (prompts::rejected(&path, &findings), Next::Continue);
                }
                let result = prompts::rejected_again(&path, &findings, &repeated);
                let boundary = Boundary {
                    items: repeated,
                    cause: BoundaryCause::RejectedTwice {
                        path,
                        content: change.new_content().to_owned(),
                    },
                };
                (result, Next::Stop(TurnEnd::Boundary(boundary)))
            }
            WriteOutcome::ParseError(error) if turn.compile_fixes == COMPILE_FIX_BUDGET => (
                prompts::parse_error_over_budget(&path, &error, COMPILE_FIX_BUDGET),
                Next::Stop(TurnEnd::CompileFixesExhausted),
            ),
            WriteOutcome::ParseError(error) => {
                turn.compile_fixes += 1;
                (prompts::parse_error(&path, &error), Next::Continue)
            }
        }
    }

    fn prepare(&self, name: &str, arguments: Map<String, Value>) -> Result<FileChange, ToolError> {
        let invalid = |error: serde_json::Error| ToolError::InvalidArguments {
            tool: name.to_owned(),
            reason: error.to_string(),
        };
        if name == super::WRITE_FILE {
            let args: WriteFileArgs =
                serde_json::from_value(Value::Object(arguments)).map_err(invalid)?;
            self.tools.prepare_write_file(&args)
        } else {
            let args: EditFileArgs =
                serde_json::from_value(Value::Object(arguments)).map_err(invalid)?;
            self.tools.prepare_edit_file(&args)
        }
    }
}

/// `request_boundary`: the model judges that the request is hard without `items`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestBoundaryArgs {
    items: Vec<String>,
    reason: String,
    proposed_code: Option<String>,
}

/// Runs `request_boundary`: stops the turn at a boundary on the items it names that `profile`
/// does not allow. A `known` item never becomes a boundary again (PRD §3 Idempotence). Arguments that name no item outside the profile, or a string that is
/// not an element name or `crate:<name>`, are returned to the model as an error, and the turn
/// goes on.
pub(super) fn request_boundary(profile: &Profile, arguments: Map<String, Value>) -> (String, Next) {
    let invalid = |reason: String| {
        let error = ToolError::InvalidArguments {
            tool: super::REQUEST_BOUNDARY.to_owned(),
            reason,
        };
        (prompts::tool_error(error), Next::Continue)
    };
    let args: RequestBoundaryArgs = match serde_json::from_value(Value::Object(arguments)) {
        Ok(args) => args,
        Err(error) => return invalid(error.to_string()),
    };
    let mut items: Vec<ItemKey> = Vec::new();
    for item in &args.items {
        match item.parse::<ItemKey>() {
            Ok(key) if !items.contains(&key) => items.push(key),
            Ok(_) => {}
            Err(error) => return invalid(error.to_string()),
        }
    }
    if items.is_empty() {
        return invalid("items must name at least one item".to_owned());
    }
    let allowed = |item: &ItemKey| profile.status(item) == Status::Known;
    let (allowed, items): (Vec<ItemKey>, Vec<ItemKey>) = items.into_iter().partition(allowed);
    if items.is_empty() {
        let allowed = allowed
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        return (prompts::all_allowed(&allowed), Next::Continue);
    }
    let boundary = Boundary {
        items,
        cause: BoundaryCause::Requested {
            reason: args.reason,
            proposed_code: args.proposed_code,
        },
    };
    (
        prompts::boundary_requested(),
        Next::Stop(TurnEnd::Boundary(boundary)),
    )
}

/// Tests of the write path in a turn. The module path has `mainline::write`, so that
/// `cargo test mainline::write` selects them.
#[cfg(test)]
mod tests;
