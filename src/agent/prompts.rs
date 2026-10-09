//! What molt says to the model: the system prompt, the tool descriptions and the tool results
//! the loop makes up itself. They are in one place so that tuning them for a model means editing
//! this file only. They are English; the model is told to answer in the user's language
//! instead (PRD §3 Language, §8.6).

use std::fmt::Display;

/// The system prompt of the main line.
pub const MAIN_LINE_SYSTEM: &str = "\
You are molt, a coding agent for Rust working in the cargo project at the current directory.
Do what the user asks by calling the tools you are given, one step at a time.
Read the files before you talk about their content, and do not guess what they contain.
Use only the tools you are given, with arguments that are one JSON object matching the tool's schema.
When the request is done, or you cannot go further, call `finish` with a short summary for the user.
Answer in the language of the user's latest input.";

pub const LIST_FILES: &str = "List the files under a directory of the project, recursively, \
     one path per line. Skips `.git` and `target`.";
pub const LIST_FILES_PATH: &str =
    "The directory, relative to the project root. The project root when omitted.";
pub const READ_FILE: &str = "Read the content of a file in the project.";
pub const READ_FILE_PATH: &str = "The file, relative to the project root.";
pub const RUN_CARGO: &str = "Run a cargo subcommand in the project. `add` asks the user for \
     approval first and runs only if the user approves.";
pub const RUN_CARGO_SUBCOMMAND: &str = "The cargo subcommand.";
pub const RUN_CARGO_ARGS: &str =
    "Only for `add`: the name of the crate to add, as the only element. Empty otherwise.";
pub const GIT_STATUS: &str = "Show `git status` of the project.";
pub const GIT_DIFF: &str = "Show `git diff` of the project: the unstaged changes, or the \
     staged ones.";
pub const GIT_DIFF_STAGED: &str = "Show the staged changes instead of the unstaged ones.";
pub const GIT_COMMIT: &str = "Commit the staged changes. Asks the user for approval first and \
     commits only if the user approves.";
pub const GIT_COMMIT_MESSAGE: &str = "The commit message.";
pub const FINISH: &str = "End your turn. Call it when the request is done or you cannot go \
     further. Tool calls after it in the same response are not run.";
pub const FINISH_SUMMARY: &str =
    "A short summary for the user of what you did, or why you stopped.";

/// The result of `finish`.
pub const FINISHED: &str = "finished";

/// The result of a tool call the loop did not run because the turn had already stopped.
pub const NOT_RUN: &str = "not run: the turn stopped before this tool call";

/// The result of the tool call that would go over the budget of `budget` tool calls per turn.
pub fn over_budget(budget: usize) -> String {
    format!("not run: this turn has used all {budget} tool calls, so it stops here")
}

/// The result of a tool call that failed: unknown tool, arguments that do not fit, a path
/// outside the project, and so on.
pub fn tool_error(error: impl Display) -> String {
    format!("error: {error}")
}

/// The result of a tool call whose arguments are not a JSON object, asking the model to send
/// the call again. Sent at most once in a row (PRD §11).
pub fn resend_arguments(error: impl Display) -> String {
    format!(
        "error: {error}\nSend this tool call again, with arguments that are one valid JSON object."
    )
}
