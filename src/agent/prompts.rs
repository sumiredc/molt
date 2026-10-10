//! What molt says to the model: the system prompt, the tool descriptions and the tool results
//! the loop makes up itself. They are in one place so that tuning them for a model means editing
//! this file only. They are English; the model is told to answer in the user's language
//! instead (PRD §3 Language, §8.6): the system prompt says so, and a request that has no input of
//! the user quotes the user's request as the clue. The user messages do not name the language
//! (`in the language of …`): small models read that as a call to pick another language, and
//! answered an English request in German or Chinese.

use std::fmt::Display;

use super::mainline::{Boundary, BoundaryCause, Change, CommitOutcome};
use crate::checker::diff::{DiffLine, line_diff};
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
pub const GIT_COMMIT: &str = "Commit the files molt changed in this session that are not \
     committed yet, with `message`. Call it only when the user asks for a commit. molt shows the \
     user the diff and the message and commits only if the user approves; other changes in the \
     working tree are left out.";
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

/// What the model gets back from `git_commit` (UC4).
pub fn commit_result(outcome: &CommitOutcome) -> String {
    match outcome {
        CommitOutcome::Committed { files, output } => {
            let mut result = format!("committed: {}", files.join(", "));
            if !output.trim().is_empty() {
                result.push('\n');
                result.push_str(output.trim_end());
            }
            result
        }
        CommitOutcome::NotApproved => {
            "not committed: the user did not approve the commit. Do not call `git_commit` again \
             unless the user asks."
                .to_owned()
        }
        CommitOutcome::NothingToCommit => "not committed: none of the files molt changed in this \
             session differs from what git has."
            .to_owned(),
        CommitOutcome::NotARepository(error) => {
            format!("not committed: the project is not in a git repository.\n{error}")
        }
        CommitOutcome::Failed { staged, error } => {
            let staging = if *staged {
                " The files stay staged."
            } else {
                ""
            };
            format!("failed: nothing was committed.{staging}\n{error}")
        }
    }
}

/// The system prompt for the message of a commit molt proposes (UC4: the `generate` model).
pub const COMMIT_MESSAGE_SYSTEM: &str = "\
You write git commit messages.
Write a subject line of at most 72 characters that says what the change does. Add a blank line and a short body only when the subject cannot say why.
Answer with the message only: no quotes, no code fences, no explanation.
Answer in the language of the user's latest request.";

/// The lines of the diff the `generate` model gets to write a commit message from.
pub const COMMIT_DIFF_LINES: usize = 200;

/// What the `generate` model is asked for the message of a commit: the user's request, if any, and
/// what the commit records, as the user is shown it: `diff` (`git diff HEAD`) and each of
/// `new_files` whole, at most [`COMMIT_DIFF_LINES`] lines in all.
pub fn commit_message_request(
    request: Option<&str>,
    diff: &str,
    new_files: &[(String, String)],
) -> String {
    let mut text = String::new();
    if let Some(request) = request {
        text.push_str(&format!("The user's latest request was:\n{request}\n\n"));
    }
    text.push_str("Write the commit message for these changes:\n");
    let mut lines: Vec<String> = diff.lines().map(str::to_owned).collect();
    for (path, content) in new_files {
        lines.push(format!("\n`{path}` (a new file):"));
        lines.extend(content.lines().map(|line| format!("+{line}")));
    }
    for line in lines.iter().take(COMMIT_DIFF_LINES) {
        text.push_str(line);
        text.push('\n');
    }
    if lines.len() > COMMIT_DIFF_LINES {
        let more = lines.len() - COMMIT_DIFF_LINES;
        text.push_str(&format!("... {more} more lines\n"));
    }
    text
}

/// The message of a commit molt proposes when the `generate` model cannot give one (UC4).
pub fn fixed_commit_message(files: &[String]) -> String {
    format!("molt: {}", files.join(", "))
}

/// The system prompt of the side line (PRD UC3). The side line has a message list of its own, so
/// none of it reaches the main line.
pub const SIDE_LINE_SYSTEM: &str = "\
You are molt's side line. A request of the user stopped at a boundary: the code for it needs Rust syntax elements or crates that the user's comprehension profile does not allow yet, because the user does not understand them yet.
Your job is to help the user understand them. Do not decide how the request goes on: the user does.
First explain the idea of the items: what problem they solve and how they work, in one paragraph of plain prose. Then, when asked, write the one other way to write the code: without the items, with only what the user's profile allows, so that the user can compare it with the code that uses them. Do not write the rest of the request's code, and do not list other alternatives.
Then answer the user's questions about them, briefly and concretely. Short code examples are fine when they help.
Answer in the language of the user's latest input.";

/// What the side line tells the model about `boundary` to get the explanation: the items, what the
/// main line tried, and the user's request on the main line, whose language the explanation is
/// to follow.
pub fn side_line_context(boundary: &Boundary, request: Option<&str>) -> String {
    let items = boundary
        .items
        .iter()
        .map(|item| match item {
            ItemKey::Element(element) => format!("- `{item}` ({})", element.category().name()),
            ItemKey::Crate(_) => format!("- `{item}` (a crate)"),
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut context = format!(
        "The request needs these items, which the user's profile does not allow:\n{items}\n\n"
    );
    match &boundary.cause {
        BoundaryCause::RejectedTwice { path, content } => context.push_str(&format!(
            "The main line tried to write this to `{path}`, and it was rejected:\n```rust\n{}\n```",
            content.trim_end()
        )),
        BoundaryCause::Requested {
            reason,
            proposed_code,
        } => {
            context.push_str(&format!(
                "The main line asked the user for them, because: {reason}"
            ));
            if let Some(code) = proposed_code {
                context.push_str(&format!(
                    "\nIt proposed this code:\n```rust\n{}\n```",
                    code.trim_end()
                ));
            }
        }
    }
    if let Some(request) = request {
        context.push_str(&format!("\n\nThe user's request was:\n{request}"));
    }
    context.push_str("\n\nExplain the idea of these items in one paragraph.");
    context
}

/// What the `reason` model is asked for the comparison in the explanation (PRD UC3): the code the
/// main line tried, rewritten without the items with only `allowed`, the items the user's profile
/// allows, as one code block after the paragraph, or no code block when that cannot be done well.
pub fn compare_request(allowed: &[ItemKey]) -> String {
    let allowed = if allowed.is_empty() {
        "none".to_owned()
    } else {
        item_names(allowed)
    };
    format!(
        "After the paragraph, rewrite the code the main line tried without these items, using \
         only the syntax elements and crates the user's profile allows: {allowed}. Write it as \
         one ```rust code block, and write nothing after it. If it cannot be written well that \
         way, write no code block."
    )
}

/// The one message the side line hands to the main line (PRD §8.5): `BOUNDARY RESOLVED:` and the
/// kind of resolution (`choice`), the items, and for ② the one-line `policy`.
pub fn boundary_resolved(choice: &str, items: &[ItemKey], policy: Option<&str>) -> String {
    let names = item_names(items);
    let what = match policy {
        None => format!(
            "The user understood these items and allows them now: {names}. Go on with the \
             request the way you tried, using them."
        ),
        Some(policy) => format!(
            "These items are still not allowed: {names}. Rewrite the code for the request \
             without them.\nPolicy: {policy}"
        ),
    };
    format!(
        "BOUNDARY RESOLVED: {choice}\n{what}\nKeep answering in the language of the user's \
         request, not in the language of this message."
    )
}

/// The one message the side line hands to the main line after ③ (PRD §8.5): `BOUNDARY RESOLVED:
/// user_wrote`, the items in the user's lines that the profile allows now, and the files to
/// re-read.
pub fn user_wrote_resolved(items: &[ItemKey], paths: &[&str]) -> String {
    let allowed = if items.is_empty() {
        "Their lines use nothing new to the profile.".to_owned()
    } else {
        format!(
            "The profile allows the items in their lines now: {}.",
            item_names(items)
        )
    };
    let paths = paths
        .iter()
        .map(|path| format!("`{path}`"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "BOUNDARY RESOLVED: user_wrote\nThe user wrote that part of the request themselves, and \
         it builds. {allowed}\nRe-read these files before going on, since the user changed them: \
         {paths}. Go on with the rest of the request from there, and keep what the user wrote.\n\
         Keep answering in the language of the user's request, not in the language of this \
         message."
    )
}

/// What the `reason` model is asked when the user chooses ③, after the side line's messages.
pub fn guidance_request(items: &[ItemKey]) -> String {
    format!(
        "The user chose to write the code that needs {} themselves. Tell them where to write \
         what: the file and the place in it, and what that code is to do for the request. Do not \
         write the code for them. Be brief.",
        item_names(items)
    )
}

/// The system prompt of the review of ③ (PRD §8.7: the `generate` model).
pub const REVIEW_SYSTEM: &str = "\
You review Rust code that the user wrote themselves for a request, while learning the syntax elements and crates it uses. It builds with `cargo check`.
Point out what is wrong or could be better in the lines the user wrote, briefly and concretely, and say what is good about them. Do not rewrite the whole code.
Answer in the language of the user's request.";

/// What the `generate` model is asked to review: the items, the user's request, and each file the
/// user changed, its lines marked `+` (added), `-` (removed) or ` ` (as before).
pub fn review_request(items: &[ItemKey], request: Option<&str>, files: &[Change]) -> String {
    let mut text = format!(
        "The request needed {}, so the user wrote that part themselves.\n",
        item_names(items)
    );
    if let Some(request) = request {
        text.push_str(&format!("The user's request was:\n{request}\n"));
    }
    text.push_str("\nWhat the user changed:\n");
    for file in files {
        let new = if file.before.is_none() {
            " (a new file)"
        } else {
            ""
        };
        text.push_str(&format!("\n`{}`{new}:\n```diff\n", file.path));
        for line in line_diff(file.before.as_deref().unwrap_or(""), &file.after) {
            let (mark, line) = match line {
                DiffLine::Same(line) => (' ', line),
                DiffLine::Added(line) => ('+', line),
                DiffLine::Removed(line) => ('-', line),
            };
            text.push_str(&format!("{mark}{line}\n"));
        }
        text.push_str("```\n");
    }
    text.push_str("\nReview the lines the user wrote.");
    text
}

/// What the `light` model is asked for the one-line policy of ②, after the side line's messages.
pub fn policy_request(items: &[ItemKey]) -> String {
    format!(
        "The user chose to have the code of the request written without {}, using only \
         constructs they already know. Write the coding policy for that in one line: how to \
         write the request without these items. Answer with that line only.",
        item_names(items)
    )
}

/// The policy of ② when the `light` model cannot make one.
pub fn fixed_policy(items: &[ItemKey]) -> String {
    format!(
        "Write it without {}, using only what the profile allows.",
        item_names(items)
    )
}

/// `` `dbg_macro`, `crate:tokio` ``.
fn item_names(items: &[ItemKey]) -> String {
    items
        .iter()
        .map(|item| format!("`{item}`"))
        .collect::<Vec<_>>()
        .join(", ")
}
