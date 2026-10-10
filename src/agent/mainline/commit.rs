//! Committing the files molt changed (PRD UC4), and only those, with the user's approval.
//!
//! A commit starts in one of two ways: the model calls `git_commit` because the user asked, or
//! molt proposes one at the end of a turn that changed files ([`MainLine::offer_commit`]). Either
//! way the user sees the diff of each file and the message before approving, and nothing reaches
//! git's index before that.

use serde_json::{Map, Value};

use super::{Change, MainLine, Next, Progress, Turn, TurnEnd};
use crate::agent::prompts;
use crate::agent::tools::{ApprovalRequest, Approver, GitCommitArgs, ToolError};
use crate::llm::{Message, Role};
use crate::workspace::approve;

/// A commit molt asks the user to approve (UC4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitProposal {
    pub message: String,
    /// The files to commit: the files molt changed and has not committed that git sees changed,
    /// in the order first changed, each with its content from before molt first changed it (or
    /// from its last commit) and now.
    pub files: Vec<Change>,
    /// The other changes git sees, staged or not, as `git status` names them. They are neither
    /// staged nor committed.
    pub others: Vec<String>,
    /// Whether the last `cargo check` molt ran did not pass, or none ran.
    pub check_failing: bool,
}

/// What came of a commit (UC4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitOutcome {
    /// `files` were committed. `output` is what `git commit` printed.
    Committed { files: Vec<String>, output: String },
    /// The user did not approve. Nothing was staged.
    NotApproved,
    /// None of the files molt changed differs from what git has.
    NothingToCommit,
    /// git cannot read a repository at the project: what it said.
    NotARepository(String),
    /// `git add` or `git commit` failed, or git could not be run, and nothing was committed.
    /// `staged` is whether the files were staged before that; they are left so.
    Failed { staged: bool, error: String },
}

impl MainLine<'_> {
    /// Runs `git_commit`: the files molt changed in the session, this turn's included, are
    /// committed with the model's message if the user approves. When something was written since
    /// the last `cargo check`, one runs first, so that a failing build is warned about.
    pub(super) fn commit_tool(
        &mut self,
        arguments: Map<String, Value>,
        turn: &mut Turn,
        progress: &mut Progress,
        approver: &mut dyn Approver,
    ) -> (String, Next) {
        let args: GitCommitArgs = match serde_json::from_value(Value::Object(arguments)) {
            Ok(args) => args,
            Err(error) => {
                let error = ToolError::InvalidArguments {
                    tool: super::GIT_COMMIT.to_owned(),
                    reason: error.to_string(),
                };
                return (prompts::tool_error(error), Next::Continue);
            }
        };
        if progress.unchecked {
            // The end of the turn still checks: this check is for the warning only.
            let _ = self.checked();
        }
        self.update_changes(&mut turn.changes, progress);
        let mut candidates = self.uncommitted.clone();
        merge(self, &mut candidates, &turn.changes, &progress.committed);
        let outcome = match self.stageable(candidates) {
            Ok((files, others)) => {
                let outcome = self.propose(args.message, files, others, approver);
                if let CommitOutcome::Committed { files, .. } = &outcome {
                    for path in files {
                        if let Some(content) = self.tools.read(path) {
                            progress.committed.push((path.clone(), content));
                        }
                    }
                }
                outcome
            }
            Err(outcome) => outcome,
        };
        let result = prompts::commit_result(&outcome);
        turn.commits.push(outcome);
        (result, Next::Continue)
    }

    /// Proposes a commit after `turn`, the latest (UC4): when the model finished it, it changed
    /// files, the model did not call `git_commit` in it (the user's request for a commit was
    /// answered there), and the last `cargo check` passed. The message comes from the `generate`
    /// model, or is a fixed one when the model cannot give one. `None` when there is nothing to
    /// propose: those do not hold, the project is not in a git repository, or nothing molt changed
    /// differs from what git has.
    pub fn offer_commit(
        &mut self,
        turn: &Turn,
        approver: &mut dyn Approver,
    ) -> Option<CommitOutcome> {
        if !matches!(turn.end, TurnEnd::Finished)
            || turn.changes.is_empty()
            || !turn.commits.is_empty()
            || self.last_check != Some(true)
        {
            return None;
        }
        let (files, others) = self.stageable(self.uncommitted.clone()).ok()?;
        let message = self.commit_message(&files);
        Some(self.propose(message, files, others, approver))
    }

    /// Merges `changes` into the uncommitted files. A file committed in the turn (`committed`)
    /// joins from its committed content, and not at all when it is still that.
    pub(super) fn absorb(&mut self, changes: &[Change], committed: &[(String, String)]) {
        let mut uncommitted = std::mem::take(&mut self.uncommitted);
        merge(self, &mut uncommitted, changes, committed);
        self.uncommitted = uncommitted;
    }

    /// Splits what git sees changed into the `candidates` it sees changed, with their content
    /// now, and the other paths.
    fn stageable(
        &self,
        mut candidates: Vec<Change>,
    ) -> Result<(Vec<Change>, Vec<String>), CommitOutcome> {
        let status = match self.tools.git().status() {
            Ok(output) if output.success => output.stdout,
            Ok(output) => {
                return Err(CommitOutcome::NotARepository(
                    output.stderr.trim_end().to_owned(),
                ));
            }
            Err(error) => {
                return Err(CommitOutcome::Failed {
                    staged: false,
                    error: error.to_string(),
                });
            }
        };
        let changed = status_paths(&status);
        let tools = &self.tools;
        for candidate in &mut candidates {
            if let Some(now) = tools.read(&candidate.path) {
                candidate.after = now;
            }
        }
        candidates.retain(|c| changed.iter().any(|path| tools.same_file(path, &c.path)));
        if candidates.is_empty() {
            return Err(CommitOutcome::NothingToCommit);
        }
        let others = changed
            .into_iter()
            .filter(|path| !candidates.iter().any(|c| tools.same_file(path, &c.path)))
            .collect();
        Ok((candidates, others))
    }

    /// Asks the user to approve committing `files` with `message`, and stages and commits only
    /// them if the user does.
    fn propose(
        &mut self,
        message: String,
        files: Vec<Change>,
        others: Vec<String>,
        approver: &mut dyn Approver,
    ) -> CommitOutcome {
        let proposal = CommitProposal {
            message,
            files,
            others,
            check_failing: self.last_check != Some(true),
        };
        if !approver.confirm(&ApprovalRequest::GitCommit(proposal.clone())) {
            return CommitOutcome::NotApproved;
        }
        let paths: Vec<&str> = proposal.files.iter().map(|f| f.path.as_str()).collect();
        let git = self.tools.git();
        match git.add(&paths) {
            Ok(output) if output.success => {}
            Ok(output) => {
                return CommitOutcome::Failed {
                    staged: false,
                    error: printed(&output.stdout, &output.stderr),
                };
            }
            Err(error) => {
                return CommitOutcome::Failed {
                    staged: false,
                    error: error.to_string(),
                };
            }
        }
        let output = match git.commit(&proposal.message, &paths, approve()) {
            Ok(output) => output,
            Err(error) => {
                return CommitOutcome::Failed {
                    staged: true,
                    error: error.to_string(),
                };
            }
        };
        if !output.success {
            return CommitOutcome::Failed {
                staged: true,
                error: printed(&output.stdout, &output.stderr),
            };
        }
        let tools = &self.tools;
        self.uncommitted
            .retain(|c| !paths.iter().any(|path| tools.same_file(path, &c.path)));
        CommitOutcome::Committed {
            files: paths.iter().map(|path| (*path).to_owned()).collect(),
            output: output.stdout,
        }
    }

    /// The message of a commit of `files` from the `generate` model, or a fixed one when it fails
    /// or gives none (Ctrl-C while waiting for it included).
    fn commit_message(&self, files: &[Change]) -> String {
        let messages = vec![
            Message::System {
                content: prompts::COMMIT_MESSAGE_SYSTEM.to_owned(),
            },
            Message::User {
                content: prompts::commit_message_request(self.latest_request(), files),
            },
        ];
        let request = self.router.request(Role::Generate, messages);
        self.log.record_request(&request);
        self.client
            .chat(&request)
            .ok()
            .and_then(|response| response.content)
            .map(|content| unfenced(&content))
            .filter(|message| !message.is_empty())
            .unwrap_or_else(|| prompts::fixed_commit_message(files))
    }
}

/// Merges `changes` into `into`: a file already there keeps its content from before and takes the
/// new one. A file in `committed` comes in from its committed content, and not at all when it is
/// still that.
fn merge(
    main_line: &MainLine,
    into: &mut Vec<Change>,
    changes: &[Change],
    committed: &[(String, String)],
) {
    let tools = &main_line.tools;
    for change in changes {
        let before = match committed
            .iter()
            .rev()
            .find(|(path, _)| tools.same_file(path, &change.path))
        {
            Some((_, content)) if *content == change.after => continue,
            Some((_, content)) => Some(content.clone()),
            None => change.before.clone(),
        };
        match into
            .iter_mut()
            .find(|c| tools.same_file(&c.path, &change.path))
        {
            Some(existing) => existing.after = change.after.clone(),
            None => into.push(Change {
                path: change.path.clone(),
                before,
                after: change.after.clone(),
            }),
        }
    }
}

/// What a git that failed printed: stderr, or stdout when it said nothing there (`git commit` says
/// "nothing to commit" on stdout).
fn printed(stdout: &str, stderr: &str) -> String {
    let text = if stderr.trim().is_empty() {
        stdout
    } else {
        stderr
    };
    text.trim_end().to_owned()
}

/// `content` without the code fence a model may put around a message.
fn unfenced(content: &str) -> String {
    let trimmed = content.trim();
    let Some(inner) = trimmed.strip_prefix("```") else {
        return trimmed.to_owned();
    };
    let inner = inner.strip_suffix("```").unwrap_or(inner);
    // The rest of the opening line names a language, if anything.
    let inner = inner.split_once('\n').map_or("", |(_, rest)| rest);
    inner.trim().to_owned()
}

/// The paths in the output of `git status --porcelain=v1`: the new path of a rename, and paths in
/// quotes unquoted.
fn status_paths(status: &str) -> Vec<String> {
    status
        .lines()
        .filter_map(|line| line.get(3..))
        .map(|path| match path.split_once(" -> ") {
            Some((_, to)) => to,
            None => path,
        })
        .map(unquote)
        .collect()
}

/// A path as git writes it, without the quotes and escapes it adds to a path with unusual
/// characters (`core.quotePath`).
fn unquote(path: &str) -> String {
    let Some(inner) = path.strip_prefix('"').and_then(|p| p.strip_suffix('"')) else {
        return path.to_owned();
    };
    let mut bytes = Vec::new();
    let mut chars = inner.bytes().peekable();
    while let Some(byte) = chars.next() {
        if byte != b'\\' {
            bytes.push(byte);
            continue;
        }
        match chars.next() {
            Some(b'n') => bytes.push(b'\n'),
            Some(b't') => bytes.push(b'\t'),
            Some(digit @ b'0'..=b'7') => {
                let mut value = u32::from(digit - b'0');
                for _ in 0..2 {
                    if let Some(next @ b'0'..=b'7') = chars.peek().copied() {
                        value = value * 8 + u32::from(next - b'0');
                        chars.next();
                    }
                }
                bytes.push(value as u8);
            }
            Some(other) => bytes.push(other),
            None => {}
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Tests of the commit of the files molt changed. The module path has `mainline::commit`, so that
/// `cargo test commit` selects them.
#[cfg(test)]
mod tests;
