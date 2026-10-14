//! ③ on the side line (PRD UC3): molt takes the step back, and the user writes that code
//! themselves. The side line tells them where to write what, and on `/done` takes the user's lines
//! from what changed on the disk since the step was taken back, detects the items in them, runs
//! `cargo check`, and has the `generate` model review them. `/done` fails only on errors in the
//! user's lines: the steps before may not build yet.

use super::{Choice, LOCK, Resolution, SubLine};
use crate::agent::mainline::Change;
use crate::agent::prompts;
use crate::agent::tools::Tools;
use crate::checker::diff;
use crate::checker::{CheckReport, ParseError};
use crate::llm::{ChatError, Message, Role};
use crate::profile::status::ItemKey;
use crate::profile::store::Profile;
use crate::session::SessionLog;
use crate::workspace::cargo::{CargoError, CargoOutput};

/// What `/done` found (UC3 ③).
#[derive(Debug)]
pub enum Done {
    /// No file the checker can check was written since the step was taken back. The user
    /// chooses again.
    NoChange,
    /// The user's lines do not parse, `cargo check` found an error in them, or could not run. The
    /// profile is not changed: the user can fix it and send `/done` again, or choose another way.
    Failed(Failure),
    /// `cargo check` found no error in the user's lines. `resolution` goes back to the main line;
    /// `review` is for the user; `check` is the `cargo check`, shown when it failed elsewhere.
    Passed {
        resolution: Resolution,
        review: Result<String, ChatError>,
        check: Result<CargoOutput, CargoError>,
    },
    /// The user interrupted the wait for the review (Ctrl-C). The profile is not changed, and the
    /// side line is still writing.
    Interrupted,
}

/// Why `/done` did not pass.
#[derive(Debug)]
pub struct Failure {
    /// Each file of the user's that does not parse, by path relative to the root, with where.
    pub parse_errors: Vec<(String, ParseError)>,
    /// The `cargo check` run on the project. It found an error in the user's lines, or could not
    /// run, unless only `parse_errors` failed.
    pub check: Result<CargoOutput, CargoError>,
}

/// The places `cargo check` reports its errors at in `output`, as path relative to the project
/// root and 1-based line: for each `error` line, the first `--> path:line:column` after it,
/// which is the error's own place. The places of the notes under it, and of warnings, are left
/// out. An error with no place (cargo's own, such as a dependency it cannot find) is `None`.
pub(crate) fn error_places(output: &CargoOutput) -> Vec<Option<(String, usize)>> {
    let mut places = Vec::new();
    let mut open = false;
    for line in output.stderr.lines().chain(output.stdout.lines()) {
        if line.starts_with("error") {
            if open {
                places.push(None);
            }
            // `error: aborting due to …` and `error: could not compile …` sum up the others.
            open = !line.starts_with("error: aborting")
                && !line.starts_with("error: could not compile");
        } else if line.starts_with("warning") {
            if open {
                places.push(None);
            }
            open = false;
        } else if open && let Some(place) = line.trim_start().strip_prefix("--> ") {
            open = false;
            let mut parts = place.rsplitn(3, ':');
            let (_column, line, path) = (parts.next(), parts.next(), parts.next());
            places.push(match (path, line.and_then(|l| l.parse().ok())) {
                (Some(path), Some(line)) => Some((path.trim_start_matches("./").to_owned(), line)),
                _ => None,
            });
        }
    }
    if open {
        places.push(None);
    }
    places
}

impl SubLine<'_> {
    /// Whether the user chose ③ and is writing: `/done` is what they send next.
    pub fn is_writing(&self) -> bool {
        self.writing
    }

    /// Starts ③, once the main line took the step back (PRD UC3): the user's lines are told apart
    /// from the files of `tools` as they are now, and ④ is no longer offered. Only the first call
    /// does anything.
    pub fn start_writing(&mut self, tools: &Tools) {
        if !self.taken_back {
            self.taken_back = true;
            self.snapshot = tools.checkable_files();
            self.lock = tools.read(LOCK);
        }
    }

    /// Asks the `reason` model where the user is to write what in ③, and returns its guidance.
    /// The side line is writing from now on, even when the request fails, unless the user
    /// interrupted the wait for it (Ctrl-C). The request body goes to `log` (PRD §8.8).
    pub fn guide(&mut self, log: &SessionLog) -> Result<String, ChatError> {
        let instruction = prompts::guidance_request(&self.boundary.items);
        let content = if self.told() {
            instruction
        } else {
            format!("{}\n\n{instruction}", self.context)
        };
        let guidance = self.exchange(content, log);
        if !matches!(guidance, Err(ChatError::Interrupted)) {
            self.writing = true;
        }
        guidance
    }

    /// The `.rs` files and `Cargo.toml`s of the project of `tools` that differ from the snapshot
    /// (a file that is new counts as all lines; a file removed is not seen), from their content
    /// then.
    pub(super) fn user_files(&self, tools: &Tools) -> Vec<Change> {
        tools
            .checkable_files()
            .into_iter()
            .filter(|(path, now)| self.snapshot.get(path) != Some(now))
            .map(|(path, after)| Change {
                before: self.snapshot.get(&path).cloned(),
                path,
                after,
            })
            .collect()
    }

    /// `/done` of ③ on the project of `tools`. The user's lines are those of
    /// [`SubLine::user_files`]. When it passes, the outcome also carries the `Cargo.lock` at the
    /// root if it differs from the snapshot. Every item in them that `profile` does not have as
    /// `known` is detected as a write would be, then `cargo check` runs; the outcome is a
    /// resolution when the lines parse and the check finds no error in a line the user changed
    /// (PRD UC3), and the `generate` model reviews the lines. When nothing changed, the side line
    /// is no longer writing, so the user chooses again. A review that fails otherwise than by the
    /// user's interrupt does not stop the resolution. Request bodies go to `log` (PRD §8.8).
    pub(crate) fn done(&mut self, tools: &Tools, profile: &Profile, log: &SessionLog) -> Done {
        let files = self.user_files(tools);
        if files.is_empty() {
            self.writing = false;
            return Done::NoChange;
        }
        let mut items: Vec<ItemKey> = Vec::new();
        let mut parse_errors = Vec::new();
        for file in &files {
            let old = file.before.as_deref().unwrap_or("");
            match tools.check(&file.path, old, &file.after, profile) {
                Some(CheckReport::OutOfProfile(findings)) => {
                    for finding in findings {
                        if !items.contains(&finding.key) {
                            items.push(finding.key);
                        }
                    }
                }
                Some(CheckReport::ParseError(error)) => {
                    parse_errors.push((file.path.clone(), error));
                }
                Some(CheckReport::Clean) | None => {}
            }
        }
        let reviewed = files.len();
        let check = tools.cargo_check();
        // An error without a place may come of the user's change (a crate added to `Cargo.toml`
        // that cannot be found), so it counts as theirs.
        let in_user_lines = |output: &CargoOutput| {
            error_places(output).iter().any(|place| match place {
                None => true,
                Some((path, line)) => files.iter().any(|file| {
                    tools.same_file(&file.path, path)
                        && diff::changed_lines(file.before.as_deref().unwrap_or(""), &file.after)
                            .contains(*line)
                }),
            })
        };
        let passed = match &check {
            Ok(output) => output.success || !in_user_lines(output),
            Err(_) => false,
        };
        if !passed || !parse_errors.is_empty() {
            return Done::Failed(Failure {
                parse_errors,
                check,
            });
        }
        let mut files = files;
        // Added after the check, which updates the lock for a crate the user added. It is not the
        // user's lines, so it is neither detected nor reviewed, but it is part of their change.
        if let Some(now) = tools
            .read(LOCK)
            .filter(|now| self.lock.as_ref() != Some(now))
        {
            files.push(Change {
                path: LOCK.to_owned(),
                before: self.lock.clone(),
                after: now,
            });
        }
        let review = match self.review(&files[..reviewed], log) {
            Err(ChatError::Interrupted) => return Done::Interrupted,
            review => review,
        };
        Done::Passed {
            resolution: Resolution {
                choice: Choice::UserWrote,
                answers: self.boundary.items.clone(),
                items,
                policy: None,
                files,
                after_user_wrote: None,
            },
            review,
            check,
        }
    }

    /// Asks the `generate` model to review the user's lines in `files` (PRD §8.7). Nothing of the
    /// side line's message list goes with it but the items and the user's request.
    fn review(&self, files: &[Change], log: &SessionLog) -> Result<String, ChatError> {
        let messages = vec![
            Message::System {
                content: prompts::REVIEW_SYSTEM.to_owned(),
            },
            Message::User {
                content: prompts::review_request(
                    &self.boundary.items,
                    self.request.as_deref(),
                    files,
                ),
            },
        ];
        let request = self.router.request(Role::Generate, messages);
        log.record_sub_request(&request);
        Ok(self.client.chat(&request)?.content.unwrap_or_default())
    }
}

/// Tests of ③ on the side line and what the main line does with it. Named so that
/// `cargo test subline::user_wrote` selects them.
#[cfg(test)]
mod tests;
