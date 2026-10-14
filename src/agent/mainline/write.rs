//! The write path of the main line (PRD §8.4): `write_file` and `edit_file` write content made of
//! `known` items as it comes. Content with items that are not `known` is written as a step, and
//! the turn stops at a boundary right after it; content too large to be a step, and content that
//! does not parse, go back to the model as the tool result, and never to the user.

use serde_json::{Map, Value};

use super::{
    Boundary, COMPILE_FIX_BUDGET, Change, MainLine, Next, Progress, SPLIT_BUDGET, STEP_ITEMS,
    STEP_LINES, Step, Turn, TurnEnd,
};
use crate::agent::prompts;
use crate::agent::tools::{EditFileArgs, FileChange, ToolError, WriteFileArgs, WriteOutcome};
use crate::checker::diff;
use crate::profile::status::ItemKey;
use crate::session::{Detected, Event, Rejection};

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
                progress.splits = 0;
                self.wrote(&path, before, after, turn, progress);
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
                let changed =
                    diff::changed_lines(before.as_deref().unwrap_or(""), &after).numbers();
                let within = changed.len() == 1
                    || (items.len() <= STEP_ITEMS && changed.len() <= STEP_LINES);
                if !within {
                    self.log.record(&Event::Rejected {
                        path: path.clone(),
                        reason: Rejection::OverStepLimit,
                        items: items.iter().map(ToString::to_string).collect(),
                        content: after,
                    });
                    if progress.splits == SPLIT_BUDGET {
                        return (
                            prompts::split_over_budget(&path, SPLIT_BUDGET),
                            Next::Stop(TurnEnd::SplitsExhausted),
                        );
                    }
                    progress.splits += 1;
                    return (prompts::split(&path, &items, changed.len()), Next::SkipRest);
                }
                if let Err(error) = self.tools.write_step(&change) {
                    return (prompts::tool_error(error), Next::Continue);
                }
                progress.splits = 0;
                let step = Step::new(path.clone(), before.clone(), after.clone());
                self.log.record(&Event::Step {
                    path: path.clone(),
                    items: items.iter().map(ToString::to_string).collect(),
                    lines: step.lines.iter().map(|(line, _)| *line).collect(),
                });
                self.wrote(&path, before, after, turn, progress);
                (
                    prompts::step_written(&path),
                    Next::Stop(TurnEnd::Boundary(Boundary { items, step })),
                )
            }
            WriteOutcome::ParseError(error) => {
                self.log.record(&Event::Rejected {
                    path: path.clone(),
                    reason: Rejection::ParseError,
                    items: Vec::new(),
                    content: after,
                });
                if turn.compile_fixes == COMPILE_FIX_BUDGET {
                    return (
                        prompts::parse_error_over_budget(&path, &error, COMPILE_FIX_BUDGET),
                        Next::Stop(TurnEnd::CompileFixesExhausted),
                    );
                }
                turn.compile_fixes += 1;
                (prompts::parse_error(&path, &error), Next::Continue)
            }
        }
    }

    /// Counts the write of `after` over `before` at `path` in the turn: among the files written,
    /// and among the changes, from the content before the turn first changed the file.
    fn wrote(
        &self,
        path: &str,
        before: Option<String>,
        after: String,
        turn: &mut Turn,
        progress: &mut Progress,
    ) {
        if !turn.written.iter().any(|p| p == path) {
            turn.written.push(path.to_owned());
        }
        let tools = &self.tools;
        match turn
            .changes
            .iter_mut()
            .find(|c| tools.same_file(&c.path, path))
        {
            Some(change) => change.after = after,
            None => turn.changes.push(Change {
                path: path.to_owned(),
                before,
                after,
            }),
        }
        progress.unchecked = true;
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

#[cfg(test)]
mod tests;
