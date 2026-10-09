//! What molt says to the model: the system prompt, the tool descriptions and the tool results
//! the loop makes up itself. They are in one place so that tuning them for a model means editing
//! this file only. They are English; the model is told to answer in the user's language
//! instead (PRD §3 Language, §8.6).

use std::fmt::Display;

use crate::checker::{Finding, ParseError};
use crate::profile::status::ItemKey;

/// The system prompt of the main line.
pub const MAIN_LINE_SYSTEM: &str = "\
You are molt, a coding agent for Rust working in the cargo project at the current directory.
Do what the user asks by calling the tools you are given, one step at a time.
Read the files before you talk about their content, and do not guess what they contain.
Use only the tools you are given, with arguments that are one JSON object matching the tool's schema.
Every `write_file` and `edit_file` is checked against the user's comprehension profile before it reaches the disk: content using a syntax element or crate outside the profile is rejected and not written.
After a rejection, rewrite the content with only what the profile allows. If the request cannot be done well without those items, call `request_boundary` instead.
When the request is done, or you cannot go further, call `finish` with a short summary for the user.
If files were written, molt runs `cargo check` after you finish and gives you its errors to fix.
Answer in the language of the user's latest input.";

pub const LIST_FILES: &str = "List the files under a directory of the project, recursively, \
     one path per line. Skips `.git` and `target`.";
pub const LIST_FILES_PATH: &str =
    "The directory, relative to the project root. The project root when omitted.";
pub const READ_FILE: &str = "Read the content of a file in the project.";
pub const READ_FILE_PATH: &str = "The file, relative to the project root.";
pub const WRITE_FILE: &str = "Write the whole content of a file in the project, creating it and \
     its directories if needed. The content is checked against the user's profile first, and \
     written only if it passes.";
pub const WRITE_FILE_PATH: &str = "The file, relative to the project root.";
pub const WRITE_FILE_CONTENT: &str = "The whole new content of the file.";
pub const EDIT_FILE: &str = "Replace the one place in an existing file where `old_string` \
     occurs with `new_string`. The edited content is checked against the user's profile first, \
     and written only if it passes.";
pub const EDIT_FILE_PATH: &str = "The file, relative to the project root.";
pub const EDIT_FILE_OLD_STRING: &str =
    "The exact text to replace. It must occur exactly once in the file.";
pub const EDIT_FILE_NEW_STRING: &str = "The text to put in its place.";
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
pub const REQUEST_BOUNDARY: &str = "Stop and hand the request to the user when it cannot be \
     done well without items outside the user's profile. The user then chooses how to go on. \
     Call it instead of writing code that you know uses such items.";
pub const REQUEST_BOUNDARY_ITEMS: &str = "The items the request needs: element names as the \
     rejections name them (`clone`, `arc`), or `crate:<name>` for a crate.";
pub const REQUEST_BOUNDARY_REASON: &str =
    "Why the request is hard without these items, in one or two sentences.";
pub const REQUEST_BOUNDARY_PROPOSED_CODE: &str =
    "The code you would write with these items, if you have it.";
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

/// The result of `write_file` or `edit_file` whose content passed the check and was written.
pub fn written(path: &str) -> String {
    format!("written: {path}")
}

/// The result of `write_file` or `edit_file` whose content uses items outside the profile: each
/// item with its line and category, and what to do instead (PRD §8.4). The file was not written.
pub fn rejected(path: &str, findings: &[Finding]) -> String {
    format!(
        "rejected: {path} was not written, because the content uses items outside the user's profile:\n{}\n\
         Rewrite the content without these items, using only what the profile allows. \
         If the request cannot be done well without them, call `request_boundary` with them instead.",
        finding_lines(findings)
    )
}

/// The result of `write_file` or `edit_file` whose content repeats `repeated`, items already
/// rejected in this turn. The file was not written, and the turn stops at a boundary.
pub fn rejected_again(path: &str, findings: &[Finding], repeated: &[ItemKey]) -> String {
    let repeated = repeated
        .iter()
        .map(|key| format!("`{key}`"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "rejected again: {path} was not written, because the content uses items outside the user's profile:\n{}\n\
         Already rejected before in this turn: {repeated}. So the turn stops here at a boundary. \
         {AT_BOUNDARY}",
        finding_lines(findings)
    )
}

/// The result of `request_boundary`.
pub fn boundary_requested() -> String {
    format!("boundary: the turn stops here. {AT_BOUNDARY}")
}

/// What the model is told at a boundary. The user may withdraw the request there, so the next
/// user message is not always about it.
const AT_BOUNDARY: &str = "The user decides how to go on: the next user message either says \
     how, or is a new request that replaces this one.";

/// The result of `request_boundary` naming only `allowed`, items the profile already allows.
/// The turn goes on.
pub fn all_allowed(allowed: &str) -> String {
    format!(
        "not a boundary: the user's profile already allows {allowed}. Write the code with them."
    )
}

/// The result of `write_file` or `edit_file` whose content does not parse. Like a compile
/// error, it is for the model to fix (PRD §8.3, §11). The file was not written.
pub fn parse_error(path: &str, error: &ParseError) -> String {
    format!(
        "parse error: {path} was not written, because the content does not parse at line {}, column {}: {}\n\
         Fix the content and write the file again.",
        error.line, error.column, error.message
    )
}

/// `- line 3: clone (ownership) in `x.clone()``, one per finding.
fn finding_lines(findings: &[Finding]) -> String {
    findings
        .iter()
        .map(|finding| {
            let category = finding
                .category()
                .map_or("crates", |category| category.name());
            format!(
                "- line {}: {} ({category}) in `{}`",
                finding.line, finding.key, finding.text
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The result of a tool call whose arguments are not a JSON object, asking the model to send
/// the call again. Sent at most once in a row (PRD §11).
pub fn resend_arguments(error: impl Display) -> String {
    format!(
        "error: {error}\nSend this tool call again, with arguments that are one valid JSON object."
    )
}

/// The result of `write_file` or `edit_file` whose content does not parse, when the model has
/// already been asked for `budget` compile fixes in this turn. The file was not written, and the
/// turn stops (PRD §8.4).
pub fn parse_error_over_budget(path: &str, error: &ParseError, budget: usize) -> String {
    format!(
        "parse error: {path} was not written, because the content does not parse at line {}, column {}: {}\n\
         This turn has used all {budget} compile fixes, so it stops here.",
        error.line, error.column, error.message
    )
}

/// The lines of cargo's output the model gets back after a failed `cargo check` at the end of a
/// turn. Local models have small contexts; the user is shown all of it.
pub const CHECK_OUTPUT_LINES: usize = 80;

/// The result of the `cargo check` molt runs at the end of a turn that wrote, when it fails
/// (PRD §8.4): that the turn goes on, what cargo printed (at most [`CHECK_OUTPUT_LINES`] lines),
/// `notes` on the crates molt could not add, and what to do.
pub fn check_failed(stdout: &str, stderr: &str, notes: &[String]) -> String {
    let mut result = String::from(
        "failed: the turn is not over. molt ran cargo check because files were written in this \
         turn, and it failed.",
    );
    let printed: Vec<&str> = [stdout, stderr]
        .into_iter()
        .flat_map(str::lines)
        .filter(|line| !line.trim().is_empty())
        .collect();
    for line in printed.iter().take(CHECK_OUTPUT_LINES) {
        result.push('\n');
        result.push_str(line);
    }
    if printed.len() > CHECK_OUTPUT_LINES {
        let more = printed.len() - CHECK_OUTPUT_LINES;
        result.push_str(&format!("\n... {more} more lines"));
    }
    for note in notes {
        result.push('\n');
        result.push_str(note);
    }
    result.push_str(
        "\nFix the errors, then call `finish` again. If they cannot be fixed within the user's \
         profile, call `finish` and say why.",
    );
    result
}

/// A note for the model that the user did not approve adding `name`.
pub fn crate_not_approved(name: &str) -> String {
    format!(
        "note: the user did not approve `cargo add {name}`, so it is not a dependency. \
         Do not call `run_cargo add` for it: write the code without it, or call `finish` and \
         tell the user."
    )
}

/// A note for the model that `cargo add name` failed.
pub fn crate_not_added(name: &str, why: &str) -> String {
    format!(
        "note: `cargo add {name}` failed: {why}\nDo not call `run_cargo add` for it: write the \
         code without it, or call `finish` and tell the user."
    )
}
