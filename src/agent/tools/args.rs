//! The arguments of each tool, read from the JSON object the model sends. A field the tool does
//! not have is rejected rather than ignored, so a misspelt option (`cached` for `staged`) is
//! reported to the model instead of silently changing what the tool does.

use serde::Deserialize;

/// `list_files`: the files under a directory, recursively.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListFilesArgs {
    /// The directory to list, relative to the project root. The root when absent.
    pub path: Option<String>,
}

/// `read_file`: the content of a file.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadFileArgs {
    pub path: String,
}

/// `write_file`: replaces the whole content of a file, or creates it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriteFileArgs {
    pub path: String,
    pub content: String,
}

/// `edit_file`: replaces the one place in a file where `old_string` occurs with `new_string`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditFileArgs {
    pub path: String,
    pub old_string: String,
    pub new_string: String,
}

/// `run_cargo`: one of the allowed cargo subcommands.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunCargoArgs {
    pub subcommand: String,
    /// Only `add` takes one: the crate name.
    #[serde(default)]
    pub args: Vec<String>,
}

/// `git_status`: takes no arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitStatusArgs {}

/// `git_diff`: the unstaged changes, or the staged ones.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitDiffArgs {
    #[serde(default)]
    pub staged: bool,
}

/// `git_commit`: commits what is staged.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitCommitArgs {
    pub message: String,
}
