//! The tools the model calls (PRD §8.4): a tool name and its JSON arguments in, an operation on
//! the workspace out.
//!
//! [`Tools::dispatch`] runs the tools that do not write files. `write_file` and `edit_file` are
//! not dispatched here: they are split into preparing a [`FileChange`] and [`Tools::write`], which
//! checks its content against the profile and writes it only when the check finds it Clean.

mod args;

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::checker::{self, CheckContext, CheckReport, Finding, ParseError};
use crate::llm::UnknownTool;
use crate::profile::store::Profile;
use crate::workspace::approve;
use crate::workspace::cargo::{
    CargoError, CargoOutput, CargoRequest, CargoRunner, CrateName, RequestError, Subcommand,
};
use crate::workspace::git::{DiffScope, Git, GitError, GitOutput};
use crate::workspace::modules::ModuleScope;
use crate::workspace::path::{PathError, Root};

pub use args::{
    EditFileArgs, GitCommitArgs, GitDiffArgs, GitStatusArgs, ListFilesArgs, ReadFileArgs,
    RunCargoArgs, WriteFileArgs,
};

/// The tools [`Tools::dispatch`] runs.
const DISPATCHED: [&str; 6] = [
    "list_files",
    "read_file",
    "run_cargo",
    "git_status",
    "git_diff",
    "git_commit",
];

/// An operation that runs only after the user approves it (PRD §3, §8.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalRequest {
    CargoAdd(CrateName),
    GitCommit { message: String },
}

/// Asks the user whether to run an operation that needs approval.
pub trait Approver {
    /// Whether the user approves `request`.
    fn confirm(&mut self, request: &ApprovalRequest) -> bool;
}

/// Why a tool did nothing. Each is returned to the model as the tool's result.
#[derive(Debug)]
pub enum ToolError {
    /// The name is not one of the dispatched tools. Lists the valid names (PRD §11).
    UnknownTool(UnknownTool),
    /// The arguments do not fit the tool's arguments.
    InvalidArguments { tool: String, reason: String },
    /// The path is outside the root, or a write is under `.git/` or `target/`.
    Path(PathError),
    /// Reading or writing a file failed, e.g. it does not exist or is not UTF-8.
    Io { path: String, source: io::Error },
    /// A write to a file the checker cannot check: neither Rust source (`.rs`) nor a
    /// `Cargo.toml`. Its content could reach the build unchecked (`#[path = "x.txt"] mod x;`,
    /// `.cargo/config.toml`), so it is not written at all.
    NotCheckable { path: String },
    /// `old_string` of `edit_file` occurs `count` times instead of exactly once.
    EditNotUnique { path: String, count: usize },
    /// The cargo subcommand or its arguments are not allowed.
    CargoRequest(RequestError),
    /// cargo could not be run.
    Cargo(CargoError),
    /// git could not be run.
    Git(GitError),
    /// The user did not approve the operation.
    NotApproved,
}

/// The tools on one project root.
pub struct Tools<'a> {
    root: Root,
    cargo: &'a dyn CargoRunner,
    git: Git,
}

/// A write of a file under the root, prepared but not yet done. Only [`Tools::prepare_write_file`]
/// and [`Tools::prepare_edit_file`] make one, so its path has been checked for writing.
#[derive(Debug)]
pub struct FileChange {
    path: String,
    resolved: PathBuf,
    old_content: Option<String>,
    new_content: String,
}

impl FileChange {
    /// The path as the model gave it.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The content on disk now, or `None` for a new file.
    pub fn old_content(&self) -> Option<&str> {
        self.old_content.as_deref()
    }

    /// The content to write.
    pub fn new_content(&self) -> &str {
        &self.new_content
    }
}

/// What [`Tools::write`] did with a [`FileChange`].
#[derive(Debug)]
pub enum WriteOutcome {
    /// Every item is a base element or `known`, and the content was written.
    Written,
    /// The content uses items that are neither base elements nor `known` (in line order, never
    /// empty). Nothing was written. `change` is handed back for what was tried, which a boundary
    /// carries (UC3).
    OutOfProfile {
        findings: Vec<Finding>,
        change: FileChange,
    },
    /// The content does not parse. Nothing was written.
    ParseError(ParseError),
}

impl<'a> Tools<'a> {
    /// Runs cargo through `cargo`, and git in `root`.
    pub fn new(root: Root, cargo: &'a dyn CargoRunner) -> Self {
        let git = Git::new(root.clone());
        Self { root, cargo, git }
    }

    /// Runs the tool `name` with `arguments`. `approver` is asked before `run_cargo add` and
    /// `git_commit`, and neither runs unless it approves. `write_file`, `edit_file` and any
    /// other name are rejected as unknown tools.
    pub fn dispatch(
        &self,
        name: &str,
        arguments: &Map<String, Value>,
        approver: &mut dyn Approver,
    ) -> Result<String, ToolError> {
        match name {
            "list_files" => self.list_files(parse(name, arguments)?),
            "read_file" => self.read_file(parse(name, arguments)?),
            "run_cargo" => self.run_cargo(parse(name, arguments)?, approver),
            "git_status" => {
                let GitStatusArgs {} = parse(name, arguments)?;
                Ok(git_result(self.git.status()?))
            }
            "git_diff" => {
                let args: GitDiffArgs = parse(name, arguments)?;
                let scope = if args.staged {
                    DiffScope::Staged
                } else {
                    DiffScope::Unstaged
                };
                Ok(git_result(self.git.diff(scope)?))
            }
            "git_commit" => self.git_commit(parse(name, arguments)?, approver),
            _ => Err(ToolError::UnknownTool(UnknownTool {
                name: name.to_owned(),
                valid: DISPATCHED.iter().map(|name| (*name).to_owned()).collect(),
            })),
        }
    }

    /// Prepares writing `args.content` to `args.path`, which may not exist yet. Nothing is
    /// written until [`Tools::write`].
    pub fn prepare_write_file(&self, args: &WriteFileArgs) -> Result<FileChange, ToolError> {
        let resolved = self.resolve_checkable(&args.path)?;
        let old_content = match fs::read_to_string(&resolved) {
            Ok(content) => Some(content),
            Err(source) if source.kind() == io::ErrorKind::NotFound => None,
            Err(source) => return Err(io_error(&args.path, source)),
        };
        Ok(FileChange {
            path: args.path.clone(),
            resolved,
            old_content,
            new_content: args.content.clone(),
        })
    }

    /// Prepares replacing `args.old_string` with `args.new_string` in the existing file
    /// `args.path`. Fails unless `old_string` occurs exactly once, since replacing one of
    /// several would change a place the model did not mean. Nothing is written until
    /// [`Tools::write`].
    pub fn prepare_edit_file(&self, args: &EditFileArgs) -> Result<FileChange, ToolError> {
        if args.old_string.is_empty() {
            return Err(ToolError::InvalidArguments {
                tool: "edit_file".to_owned(),
                reason: "old_string must not be empty".to_owned(),
            });
        }
        let resolved = self.resolve_checkable(&args.path)?;
        let old_content =
            fs::read_to_string(&resolved).map_err(|source| io_error(&args.path, source))?;
        let new_content =
            replace_once(&old_content, &args.old_string, &args.new_string).map_err(|count| {
                ToolError::EditNotUnique {
                    path: args.path.clone(),
                    count,
                }
            })?;
        Ok(FileChange {
            path: args.path.clone(),
            resolved,
            old_content: Some(old_content),
            new_content,
        })
    }

    /// Checks the content of `change` against `profile` (PRD §8.4) and writes it, creating the
    /// missing parent directories, only when the check finds it Clean. Only the lines changed
    /// from the content on disk are checked. The project's own modules are read from the
    /// project as it is now, since the model may have added one earlier in the turn.
    pub fn write(&self, change: FileChange, profile: &Profile) -> Result<WriteOutcome, ToolError> {
        let own_modules = ModuleScope::of(&change.resolved).own_modules();
        let ctx = CheckContext {
            profile,
            own_modules: &own_modules,
        };
        let report = checker::check_file(
            &change.resolved,
            change.old_content().unwrap_or(""),
            change.new_content(),
            &ctx,
        );
        match report {
            CheckReport::Clean => {}
            CheckReport::OutOfProfile(findings) => {
                return Ok(WriteOutcome::OutOfProfile { findings, change });
            }
            CheckReport::ParseError(error) => {
                return Ok(WriteOutcome::ParseError(error));
            }
        }
        if let Some(parent) = change.resolved.parent() {
            fs::create_dir_all(parent).map_err(|source| io_error(&change.path, source))?;
        }
        fs::write(&change.resolved, &change.new_content)
            .map_err(|source| io_error(&change.path, source))?;
        Ok(WriteOutcome::Written)
    }

    /// Runs `cargo check` in the project, as the loop does at the end of a turn that wrote
    /// (PRD §8.4).
    pub(crate) fn cargo_check(&self) -> Result<CargoOutput, CargoError> {
        self.cargo.run(Subcommand::Check)
    }

    /// Runs `cargo add crate_name` if `approver` approves it (PRD §8.4).
    pub(crate) fn add_crate(
        &self,
        crate_name: &CrateName,
        approver: &mut dyn Approver,
    ) -> Result<CargoOutput, ToolError> {
        if !approver.confirm(&ApprovalRequest::CargoAdd(crate_name.clone())) {
            return Err(ToolError::NotApproved);
        }
        Ok(self.cargo.add(crate_name, approve())?)
    }

    /// The crates the Rust files `paths` use that `profile` allows but the `Cargo.toml` at the
    /// root does not declare (PRD UC2), each once, in the order first used. Files that cannot be
    /// read, and files that are not Rust, use none.
    pub(crate) fn undeclared_known_crates(
        &self,
        paths: &[String],
        profile: &Profile,
    ) -> Vec<String> {
        let manifest = self.read("Cargo.toml").unwrap_or_default();
        let mut names: Vec<String> = Vec::new();
        for path in paths {
            let Ok(resolved) = self.root.resolve_read(path) else {
                continue;
            };
            if resolved.extension().is_none_or(|ext| ext != "rs") {
                continue;
            }
            let Ok(source) = fs::read_to_string(&resolved) else {
                continue;
            };
            let own_modules = ModuleScope::of(&resolved).own_modules();
            let ctx = CheckContext {
                profile,
                own_modules: &own_modules,
            };
            for name in checker::undeclared_known_crates(&source, &manifest, &ctx) {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
        names
    }

    /// Whether `a` and `b` name the same file under the root for writing (`./Cargo.toml` and
    /// `Cargo.toml`). Paths that cannot be written are no file.
    pub(crate) fn same_file(&self, a: &str, b: &str) -> bool {
        match (self.root.resolve_write(a), self.root.resolve_write(b)) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
    }

    /// The content of `path` in the project, or `None` when it cannot be read.
    pub(crate) fn read(&self, path: &str) -> Option<String> {
        let resolved = self.root.resolve_read(path).ok()?;
        fs::read_to_string(resolved).ok()
    }

    /// The files under the root that the checker can check (`.rs` files and `Cargo.toml`s), with
    /// their content, by path relative to the root. Skips what `list_files` skips, files that are
    /// not UTF-8, and directories that cannot be read.
    pub(crate) fn checkable_files(&self) -> BTreeMap<String, String> {
        let mut paths = Vec::new();
        // A directory that cannot be read leaves out its files; the rest are still taken.
        let _ = self.walk(self.root.as_path(), &mut paths);
        paths
            .into_iter()
            .filter(|path| is_checkable(Path::new(path)))
            .filter_map(|path| self.read(&path).map(|content| (path, content)))
            .collect()
    }

    /// Resolves `path` for writing, if the checker can check it: a `.rs` file or a `Cargo.toml`.
    fn resolve_checkable(&self, path: &str) -> Result<PathBuf, ToolError> {
        let resolved = self.root.resolve_write(path)?;
        if !is_checkable(&resolved) {
            return Err(ToolError::NotCheckable {
                path: path.to_owned(),
            });
        }
        Ok(resolved)
    }

    /// The files under `args.path`, one per line, relative to the root and sorted. Does not
    /// descend into `.git` or the root's `target`, nor into a symlinked directory, which may
    /// point outside the root.
    fn list_files(&self, args: ListFilesArgs) -> Result<String, ToolError> {
        let path = args.path.as_deref().unwrap_or(".");
        let dir = self.root.resolve_read(path)?;
        let mut files = Vec::new();
        self.walk(&dir, &mut files)?;
        files.sort();
        Ok(files.join("\n"))
    }

    fn walk(&self, dir: &Path, files: &mut Vec<String>) -> Result<(), ToolError> {
        let io = |source| io_error(&self.relative(dir), source);
        for entry in fs::read_dir(dir).map_err(io)? {
            let entry = entry.map_err(io)?;
            let path = entry.path();
            if entry.file_type().map_err(io)?.is_dir() {
                if !self.is_skipped(&path) {
                    self.walk(&path, files)?;
                }
            } else {
                files.push(self.relative(&path));
            }
        }
        Ok(())
    }

    /// Whether `dir` is git's data or cargo's output, which are noise in a listing.
    fn is_skipped(&self, dir: &Path) -> bool {
        let Ok(relative) = dir.strip_prefix(self.root.as_path()) else {
            return false;
        };
        let names: Vec<_> = relative.components().collect();
        match names.as_slice() {
            [Component::Normal(name)] if name.eq_ignore_ascii_case("target") => true,
            [.., Component::Normal(name)] => name.eq_ignore_ascii_case(".git"),
            _ => false,
        }
    }

    fn relative(&self, path: &Path) -> String {
        let relative = path.strip_prefix(self.root.as_path()).unwrap_or(path);
        if relative.as_os_str().is_empty() {
            return ".".to_owned();
        }
        relative.to_string_lossy().into_owned()
    }

    fn read_file(&self, args: ReadFileArgs) -> Result<String, ToolError> {
        let resolved = self.root.resolve_read(&args.path)?;
        fs::read_to_string(resolved).map_err(|source| io_error(&args.path, source))
    }

    /// Checks the request against the allowlist before asking for approval, so the user is not
    /// asked about a request that would be rejected anyway.
    fn run_cargo(
        &self,
        args: RunCargoArgs,
        approver: &mut dyn Approver,
    ) -> Result<String, ToolError> {
        let output = match CargoRequest::parse(&args.subcommand, &args.args)? {
            CargoRequest::Run(subcommand) => self.cargo.run(subcommand)?,
            CargoRequest::Add(crate_name) => self.add_crate(&crate_name, approver)?,
        };
        Ok(process_result(
            output.success,
            &output.stdout,
            &output.stderr,
        ))
    }

    fn git_commit(
        &self,
        args: GitCommitArgs,
        approver: &mut dyn Approver,
    ) -> Result<String, ToolError> {
        let request = ApprovalRequest::GitCommit {
            message: args.message.clone(),
        };
        if !approver.confirm(&request) {
            return Err(ToolError::NotApproved);
        }
        Ok(git_result(self.git.commit(&args.message, approve())?))
    }
}

fn parse<T: DeserializeOwned>(tool: &str, arguments: &Map<String, Value>) -> Result<T, ToolError> {
    serde_json::from_value(Value::Object(arguments.clone())).map_err(|source| {
        ToolError::InvalidArguments {
            tool: tool.to_owned(),
            reason: source.to_string(),
        }
    })
}

/// Replaces the only occurrence of `old` in `content`, or returns how many there are.
/// Overlapping occurrences count separately: in `aaa`, `aa` occurs twice.
fn replace_once(content: &str, old: &str, new: &str) -> Result<String, usize> {
    let starts: Vec<usize> = content
        .char_indices()
        .map(|(index, _)| index)
        .filter(|&index| content[index..].starts_with(old))
        .collect();
    let [start] = starts[..] else {
        return Err(starts.len());
    };
    let mut replaced = String::with_capacity(content.len() - old.len() + new.len());
    replaced.push_str(&content[..start]);
    replaced.push_str(new);
    replaced.push_str(&content[start + old.len()..]);
    Ok(replaced)
}

/// Whether the checker can check the file at `path`: a `.rs` file or a `Cargo.toml`.
fn is_checkable(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == "rs")
        || path.file_name().is_some_and(|name| name == "Cargo.toml")
}

fn io_error(path: &str, source: io::Error) -> ToolError {
    ToolError::Io {
        path: path.to_owned(),
        source,
    }
}

fn git_result(output: GitOutput) -> String {
    process_result(output.success, &output.stdout, &output.stderr)
}

/// Whether the process succeeded, then what it printed.
fn process_result(success: bool, stdout: &str, stderr: &str) -> String {
    let mut result = String::from(if success { "succeeded" } else { "failed" });
    for (label, text) in [("stdout", stdout), ("stderr", stderr)] {
        if !text.is_empty() {
            result.push_str(&format!("\n--- {label} ---\n{text}"));
        }
    }
    result
}

impl From<PathError> for ToolError {
    fn from(source: PathError) -> Self {
        Self::Path(source)
    }
}

impl From<RequestError> for ToolError {
    fn from(source: RequestError) -> Self {
        Self::CargoRequest(source)
    }
}

impl From<CargoError> for ToolError {
    fn from(source: CargoError) -> Self {
        Self::Cargo(source)
    }
}

impl From<GitError> for ToolError {
    fn from(source: GitError) -> Self {
        Self::Git(source)
    }
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownTool(source) => write!(f, "{source}"),
            Self::InvalidArguments { tool, reason } => {
                write!(f, "invalid arguments for tool `{tool}`: {reason}")
            }
            Self::Path(source) => write!(f, "{source}"),
            Self::Io { path, source } => write!(f, "{path}: {source}"),
            Self::NotCheckable { path } => write!(
                f,
                "{path} cannot be written: molt writes only Rust source files (`.rs`) and `Cargo.toml`, whose content it can check"
            ),
            Self::EditNotUnique { path, count: 0 } => write!(
                f,
                "old_string does not occur in {path}: read the file and copy the text exactly"
            ),
            Self::EditNotUnique { path, count } => write!(
                f,
                "old_string occurs {count} times in {path}: include more of the surrounding text so it occurs exactly once"
            ),
            Self::CargoRequest(source) => write!(f, "{source}"),
            Self::Cargo(source) => write!(f, "{source}"),
            Self::Git(source) => write!(f, "{source}"),
            Self::NotApproved => write!(f, "the user did not approve this operation"),
        }
    }
}

impl Error for ToolError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::UnknownTool(source) => Some(source),
            Self::Path(source) => Some(source),
            Self::Io { source, .. } => Some(source),
            Self::CargoRequest(source) => Some(source),
            Self::Cargo(source) => Some(source),
            Self::Git(source) => Some(source),
            Self::InvalidArguments { .. }
            | Self::NotCheckable { .. }
            | Self::EditNotUnique { .. }
            | Self::NotApproved => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    use crate::workspace::cargo::{CargoCall, CargoOutput, FakeCargo, Subcommand};
    use crate::workspace::git::testing::{git_in, init};

    /// Answers every request the same way and records what it was asked.
    struct Answer {
        approve: bool,
        asked: Vec<ApprovalRequest>,
    }

    impl Answer {
        fn yes() -> Self {
            Self {
                approve: true,
                asked: Vec::new(),
            }
        }

        fn no() -> Self {
            Self {
                approve: false,
                asked: Vec::new(),
            }
        }
    }

    impl Approver for Answer {
        fn confirm(&mut self, request: &ApprovalRequest) -> bool {
            self.asked.push(request.clone());
            self.approve
        }
    }

    fn arguments(value: Value) -> Map<String, Value> {
        let Value::Object(map) = value else {
            panic!("not an object: {value}");
        };
        map
    }

    /// A project root with a few files, including some under `.git` and `target`.
    fn project() -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, content) in [
            ("Cargo.toml", "[package]\n"),
            ("src/main.rs", "fn main() {}\n"),
            ("src/target/mod.rs", ""),
            (".git/config", ""),
            ("target/debug/molt", ""),
        ] {
            let path = dir.path().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }
        dir
    }

    fn tools<'a>(dir: &TempDir, cargo: &'a FakeCargo) -> Tools<'a> {
        Tools::new(Root::new(dir.path()).unwrap(), cargo)
    }

    /// Writes `change` with a profile of base elements only, which its content must pass.
    fn write_clean(tools: &Tools, change: FileChange) {
        let outcome = tools.write(change, &Profile::new()).unwrap();
        assert!(matches!(outcome, WriteOutcome::Written), "{outcome:?}");
    }

    /// Tools on a new git repository in `dir`, whose git runs with the test environment.
    fn tools_on_git<'a>(dir: &TempDir, cargo: &'a FakeCargo) -> Tools<'a> {
        let git = init(dir.path());
        Tools {
            root: Root::new(dir.path()).unwrap(),
            cargo,
            git,
        }
    }

    fn read(dir: &TempDir, path: &str) -> String {
        fs::read_to_string(dir.path().join(path)).unwrap()
    }

    fn stage(dir: &TempDir, path: &str) {
        let status = git_in(dir.path(), &["add", path]).status().unwrap();
        assert!(status.success());
    }

    fn log(dir: &TempDir) -> Vec<String> {
        let output = git_in(dir.path(), &["log", "--format=%s"])
            .output()
            .unwrap();
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    // list_files

    #[test]
    fn list_files_lists_every_file_except_under_git_and_the_root_target() {
        let dir = project();
        let cargo = FakeCargo::new();

        let result = tools(&dir, &cargo).dispatch("list_files", &Map::new(), &mut Answer::no());

        assert_eq!(
            result.unwrap(),
            "Cargo.toml\nsrc/main.rs\nsrc/target/mod.rs"
        );
    }

    #[test]
    fn list_files_skips_a_nested_git_and_ignores_the_case_of_git_and_target() {
        let dir = tempfile::tempdir().unwrap();
        for path in [
            "TARGET/debug/molt",
            "vendor/dep/.GIT/config",
            "vendor/dep/lib.rs",
        ] {
            let path = dir.path().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "").unwrap();
        }
        let cargo = FakeCargo::new();

        let result = tools(&dir, &cargo).dispatch("list_files", &Map::new(), &mut Answer::no());

        assert_eq!(result.unwrap(), "vendor/dep/lib.rs");
    }

    #[test]
    fn list_files_with_a_path_lists_that_directory_relative_to_the_root() {
        let dir = project();
        let cargo = FakeCargo::new();

        let result = tools(&dir, &cargo).dispatch(
            "list_files",
            &arguments(json!({ "path": "src" })),
            &mut Answer::no(),
        );

        assert_eq!(result.unwrap(), "src/main.rs\nsrc/target/mod.rs");
    }

    #[test]
    fn list_files_outside_the_root_is_rejected() {
        let dir = project();
        let cargo = FakeCargo::new();

        let result = tools(&dir, &cargo).dispatch(
            "list_files",
            &arguments(json!({ "path": ".." })),
            &mut Answer::no(),
        );

        assert!(
            matches!(result, Err(ToolError::Path(PathError::OutsideRoot { .. }))),
            "{result:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn list_files_does_not_descend_into_a_symlinked_directory() {
        let dir = project();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret"), "").unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("src/link")).unwrap();
        let cargo = FakeCargo::new();

        let result = tools(&dir, &cargo).dispatch(
            "list_files",
            &arguments(json!({ "path": "src" })),
            &mut Answer::no(),
        );

        assert_eq!(result.unwrap(), "src/link\nsrc/main.rs\nsrc/target/mod.rs");
    }

    // read_file

    #[test]
    fn read_file_returns_the_content() {
        let dir = project();
        let cargo = FakeCargo::new();

        let result = tools(&dir, &cargo).dispatch(
            "read_file",
            &arguments(json!({ "path": "src/main.rs" })),
            &mut Answer::no(),
        );

        assert_eq!(result.unwrap(), "fn main() {}\n");
    }

    #[test]
    fn read_file_outside_the_root_is_rejected() {
        let dir = project();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret"), "secret").unwrap();
        let cargo = FakeCargo::new();

        let result = tools(&dir, &cargo).dispatch(
            "read_file",
            &arguments(json!({ "path": outside.path().join("secret") })),
            &mut Answer::no(),
        );

        assert!(
            matches!(result, Err(ToolError::Path(PathError::OutsideRoot { .. }))),
            "{result:?}"
        );
    }

    // run_cargo

    #[test]
    fn run_cargo_returns_the_result_of_an_allowed_subcommand_without_asking() {
        let dir = project();
        let cargo = FakeCargo::new();
        cargo.push(Ok(CargoOutput {
            success: false,
            stdout: String::new(),
            stderr: "error[E0308]: mismatched types\n".to_owned(),
        }));
        let mut answer = Answer::no();

        let result = tools(&dir, &cargo).dispatch(
            "run_cargo",
            &arguments(json!({ "subcommand": "check" })),
            &mut answer,
        );

        assert_eq!(
            result.unwrap(),
            "failed\n--- stderr ---\nerror[E0308]: mismatched types\n"
        );
        assert_eq!(cargo.calls(), [CargoCall::Run(Subcommand::Check)]);
        assert_eq!(answer.asked, []);
    }

    #[test]
    fn run_cargo_outside_the_allowlist_is_rejected_without_running_cargo() {
        let dir = project();
        let cargo = FakeCargo::new();

        let result = tools(&dir, &cargo).dispatch(
            "run_cargo",
            &arguments(json!({ "subcommand": "run" })),
            &mut Answer::yes(),
        );

        assert!(
            matches!(
                result,
                Err(ToolError::CargoRequest(RequestError::NotAllowed { .. }))
            ),
            "{result:?}"
        );
        assert_eq!(cargo.calls(), []);
    }

    #[test]
    fn run_cargo_add_runs_after_the_user_approves() {
        let dir = project();
        let cargo = FakeCargo::new();
        cargo.push(Ok(CargoOutput {
            success: true,
            stdout: String::new(),
            stderr: "Adding serde\n".to_owned(),
        }));
        let mut answer = Answer::yes();

        let result = tools(&dir, &cargo).dispatch(
            "run_cargo",
            &arguments(json!({ "subcommand": "add", "args": ["serde"] })),
            &mut answer,
        );

        let serde = CrateName::new("serde").unwrap();
        assert_eq!(result.unwrap(), "succeeded\n--- stderr ---\nAdding serde\n");
        assert_eq!(answer.asked, [ApprovalRequest::CargoAdd(serde.clone())]);
        assert_eq!(cargo.calls(), [CargoCall::Add(serde)]);
    }

    #[test]
    fn run_cargo_add_does_not_run_when_the_user_declines() {
        let dir = project();
        let cargo = FakeCargo::new();
        let mut answer = Answer::no();

        let result = tools(&dir, &cargo).dispatch(
            "run_cargo",
            &arguments(json!({ "subcommand": "add", "args": ["serde"] })),
            &mut answer,
        );

        assert!(matches!(result, Err(ToolError::NotApproved)), "{result:?}");
        assert_eq!(answer.asked.len(), 1);
        assert_eq!(cargo.calls(), []);
    }

    #[test]
    fn run_cargo_add_of_an_invalid_crate_name_is_rejected_without_asking() {
        let dir = project();
        let cargo = FakeCargo::new();
        let mut answer = Answer::yes();

        let result = tools(&dir, &cargo).dispatch(
            "run_cargo",
            &arguments(json!({ "subcommand": "add", "args": ["serde@1.0"] })),
            &mut answer,
        );

        assert!(
            matches!(
                result,
                Err(ToolError::CargoRequest(
                    RequestError::InvalidCrateName { .. }
                ))
            ),
            "{result:?}"
        );
        assert_eq!(answer.asked, []);
        assert_eq!(cargo.calls(), []);
    }

    // git_status, git_diff, git_commit

    #[test]
    fn git_status_returns_the_status_of_the_repository() {
        let dir = tempfile::tempdir().unwrap();
        let cargo = FakeCargo::new();
        let tools = tools_on_git(&dir, &cargo);
        fs::write(dir.path().join("new.txt"), "").unwrap();

        let result = tools.dispatch("git_status", &Map::new(), &mut Answer::no());

        assert_eq!(result.unwrap(), "succeeded\n--- stdout ---\n?? new.txt\n");
    }

    #[test]
    fn git_diff_shows_the_unstaged_or_the_staged_changes() {
        let dir = tempfile::tempdir().unwrap();
        let cargo = FakeCargo::new();
        let tools = tools_on_git(&dir, &cargo);
        fs::write(dir.path().join("a.txt"), "staged\n").unwrap();
        stage(&dir, "a.txt");
        fs::write(dir.path().join("a.txt"), "unstaged\n").unwrap();

        let unstaged = tools
            .dispatch("git_diff", &Map::new(), &mut Answer::no())
            .unwrap();
        let staged = tools
            .dispatch(
                "git_diff",
                &arguments(json!({ "staged": true })),
                &mut Answer::no(),
            )
            .unwrap();

        assert!(unstaged.contains("-staged\n+unstaged\n"), "{unstaged}");
        assert!(staged.contains("+staged\n"), "{staged}");
        assert!(!staged.contains("unstaged"), "{staged}");
    }

    #[test]
    fn git_commit_commits_what_is_staged_after_the_user_approves() {
        let dir = tempfile::tempdir().unwrap();
        let cargo = FakeCargo::new();
        let tools = tools_on_git(&dir, &cargo);
        fs::write(dir.path().join("a.txt"), "a\n").unwrap();
        stage(&dir, "a.txt");
        let mut answer = Answer::yes();

        let result = tools.dispatch(
            "git_commit",
            &arguments(json!({ "message": "Add a.txt" })),
            &mut answer,
        );

        assert!(result.unwrap().starts_with("succeeded\n"));
        assert_eq!(
            answer.asked,
            [ApprovalRequest::GitCommit {
                message: "Add a.txt".to_owned()
            }]
        );
        assert_eq!(log(&dir), ["Add a.txt"]);
        // Only the commit reached git, so nothing was pushed.
        assert_eq!(
            tools.git.invocations(),
            [vec![
                OsString::from("commit"),
                OsString::from("--message=Add a.txt")
            ]]
        );
    }

    #[test]
    fn git_commit_does_not_commit_when_the_user_declines() {
        let dir = tempfile::tempdir().unwrap();
        let cargo = FakeCargo::new();
        let tools = tools_on_git(&dir, &cargo);
        fs::write(dir.path().join("a.txt"), "a\n").unwrap();
        stage(&dir, "a.txt");

        let result = tools.dispatch(
            "git_commit",
            &arguments(json!({ "message": "Add a.txt" })),
            &mut Answer::no(),
        );

        assert!(matches!(result, Err(ToolError::NotApproved)), "{result:?}");
        assert_eq!(log(&dir), Vec::<String>::new());
        assert_eq!(tools.git.invocations(), Vec::<Vec<OsString>>::new());
    }

    // names and arguments

    #[test]
    fn write_file_and_edit_file_are_rejected_as_unknown_tools_and_change_nothing() {
        let dir = project();
        let cargo = FakeCargo::new();
        let tools = tools(&dir, &cargo);

        let write = tools.dispatch(
            "write_file",
            &arguments(json!({ "path": "new.rs", "content": "fn f() {}\n" })),
            &mut Answer::yes(),
        );
        let edit = tools.dispatch(
            "edit_file",
            &arguments(json!({
                "path": "src/main.rs",
                "old_string": "fn main() {}",
                "new_string": "fn main() { loop {} }",
            })),
            &mut Answer::yes(),
        );

        for result in [write, edit] {
            let Err(ToolError::UnknownTool(unknown)) = result else {
                panic!("not rejected as an unknown tool: {result:?}");
            };
            assert_eq!(unknown.valid, DISPATCHED);
        }
        assert!(!dir.path().join("new.rs").exists());
        assert_eq!(read(&dir, "src/main.rs"), "fn main() {}\n");
    }

    #[test]
    fn arguments_with_a_field_the_tool_does_not_have_are_rejected() {
        let dir = project();
        let cargo = FakeCargo::new();

        let result = tools(&dir, &cargo).dispatch(
            "git_diff",
            &arguments(json!({ "cached": true })),
            &mut Answer::no(),
        );

        assert!(
            matches!(&result, Err(ToolError::InvalidArguments { tool, .. }) if tool == "git_diff"),
            "{result:?}"
        );
    }

    #[test]
    fn arguments_without_a_required_field_are_rejected() {
        let dir = project();
        let cargo = FakeCargo::new();

        let result = tools(&dir, &cargo).dispatch("read_file", &Map::new(), &mut Answer::no());

        assert!(
            matches!(&result, Err(ToolError::InvalidArguments { tool, .. }) if tool == "read_file"),
            "{result:?}"
        );
    }

    // write_file and edit_file

    #[test]
    fn prepared_write_reaches_the_disk_only_when_written() {
        let dir = project();
        let cargo = FakeCargo::new();
        let tools = tools(&dir, &cargo);

        let change = tools
            .prepare_write_file(&WriteFileArgs {
                path: "src/main.rs".to_owned(),
                content: "fn main() { loop {} }\n".to_owned(),
            })
            .unwrap();

        assert_eq!(change.old_content(), Some("fn main() {}\n"));
        assert_eq!(change.new_content(), "fn main() { loop {} }\n");
        assert_eq!(read(&dir, "src/main.rs"), "fn main() {}\n");

        write_clean(&tools, change);

        assert_eq!(read(&dir, "src/main.rs"), "fn main() { loop {} }\n");
    }

    #[test]
    fn write_of_a_new_file_has_no_old_content_and_creates_its_directories() {
        let dir = project();
        let cargo = FakeCargo::new();
        let tools = tools(&dir, &cargo);

        let change = tools
            .prepare_write_file(&WriteFileArgs {
                path: "src/a/b.rs".to_owned(),
                content: "fn b() {}\n".to_owned(),
            })
            .unwrap();
        assert_eq!(change.old_content(), None);
        write_clean(&tools, change);

        assert_eq!(read(&dir, "src/a/b.rs"), "fn b() {}\n");
    }

    #[test]
    fn content_out_of_the_profile_is_not_written_and_its_findings_come_back() {
        let dir = project();
        let cargo = FakeCargo::new();
        let tools = tools(&dir, &cargo);
        let change = tools
            .prepare_write_file(&WriteFileArgs {
                path: "src/main.rs".to_owned(),
                content: "fn main() {\n    dbg!(1);\n}\n".to_owned(),
            })
            .unwrap();

        let outcome = tools.write(change, &Profile::new()).unwrap();

        let WriteOutcome::OutOfProfile { findings, change } = outcome else {
            panic!("{outcome:?}");
        };
        assert_eq!(findings[0].line, 2);
        assert_eq!(change.path(), "src/main.rs");
        assert_eq!(read(&dir, "src/main.rs"), "fn main() {}\n");
    }

    #[test]
    fn content_that_does_not_parse_is_not_written() {
        let dir = project();
        let cargo = FakeCargo::new();
        let tools = tools(&dir, &cargo);
        let change = tools
            .prepare_write_file(&WriteFileArgs {
                path: "src/new.rs".to_owned(),
                content: "fn f( {}\n".to_owned(),
            })
            .unwrap();

        let outcome = tools.write(change, &Profile::new()).unwrap();

        assert!(
            matches!(outcome, WriteOutcome::ParseError(_)),
            "{outcome:?}"
        );
        assert!(!dir.path().join("src/new.rs").exists());
    }

    #[test]
    fn a_file_that_is_neither_rust_nor_cargo_toml_cannot_be_written_or_edited() {
        let dir = project();
        fs::write(dir.path().join("README.md"), "# demo\n").unwrap();
        let cargo = FakeCargo::new();
        let tools = tools(&dir, &cargo);

        let write = tools.prepare_write_file(&WriteFileArgs {
            path: "src/x.txt".to_owned(),
            content: "fn main() {}\n".to_owned(),
        });
        let edit = tools.prepare_edit_file(&EditFileArgs {
            path: "README.md".to_owned(),
            old_string: "demo".to_owned(),
            new_string: "molt".to_owned(),
        });

        assert!(
            matches!(&write, Err(ToolError::NotCheckable { path }) if path == "src/x.txt"),
            "{write:?}"
        );
        assert!(
            matches!(&edit, Err(ToolError::NotCheckable { path }) if path == "README.md"),
            "{edit:?}"
        );
        assert!(!dir.path().join("src/x.txt").exists());
    }

    #[test]
    fn a_cargo_toml_in_a_subdirectory_can_be_written() {
        let dir = project();
        let cargo = FakeCargo::new();
        let tools = tools(&dir, &cargo);
        let change = tools
            .prepare_write_file(&WriteFileArgs {
                path: "member/Cargo.toml".to_owned(),
                content: "[package]\nname = \"member\"\n".to_owned(),
            })
            .unwrap();

        write_clean(&tools, change);

        assert_eq!(
            read(&dir, "member/Cargo.toml"),
            "[package]\nname = \"member\"\n"
        );
    }

    #[test]
    fn write_under_git_is_rejected() {
        let dir = project();
        let cargo = FakeCargo::new();

        let result = tools(&dir, &cargo).prepare_write_file(&WriteFileArgs {
            path: ".git/hooks/pre-commit".to_owned(),
            content: "#!/bin/sh\n".to_owned(),
        });

        assert!(
            matches!(
                result,
                Err(ToolError::Path(PathError::WriteForbidden { .. }))
            ),
            "{result:?}"
        );
        assert!(!dir.path().join(".git/hooks").exists());
    }

    #[test]
    fn edit_under_git_is_rejected() {
        let dir = project();
        fs::write(dir.path().join(".git/config"), "[core]\n").unwrap();
        let cargo = FakeCargo::new();

        let result = tools(&dir, &cargo).prepare_edit_file(&EditFileArgs {
            path: ".git/config".to_owned(),
            old_string: "[core]".to_owned(),
            new_string: "[core]\n\thooksPath = /tmp".to_owned(),
        });

        assert!(
            matches!(
                result,
                Err(ToolError::Path(PathError::WriteForbidden { .. }))
            ),
            "{result:?}"
        );
    }

    #[test]
    fn edit_outside_the_root_is_rejected() {
        let dir = project();
        let cargo = FakeCargo::new();

        let result = tools(&dir, &cargo).prepare_edit_file(&EditFileArgs {
            path: "../outside.rs".to_owned(),
            old_string: "a".to_owned(),
            new_string: "b".to_owned(),
        });

        assert!(
            matches!(result, Err(ToolError::Path(PathError::OutsideRoot { .. }))),
            "{result:?}"
        );
    }

    fn edit(old_string: &str, new_string: &str) -> EditFileArgs {
        EditFileArgs {
            path: "src/lib.rs".to_owned(),
            old_string: old_string.to_owned(),
            new_string: new_string.to_owned(),
        }
    }

    #[test]
    fn edit_with_one_match_replaces_only_it() {
        let dir = project();
        fs::write(dir.path().join("src/lib.rs"), "fn a() {}\nfn b() {}\n").unwrap();
        let cargo = FakeCargo::new();
        let tools = tools(&dir, &cargo);

        let change = tools.prepare_edit_file(&edit("fn b", "fn c")).unwrap();

        assert_eq!(change.old_content(), Some("fn a() {}\nfn b() {}\n"));
        assert_eq!(change.new_content(), "fn a() {}\nfn c() {}\n");
        write_clean(&tools, change);
        assert_eq!(read(&dir, "src/lib.rs"), "fn a() {}\nfn c() {}\n");
    }

    #[test]
    fn edit_with_no_match_fails_and_leaves_the_file_unchanged() {
        let dir = project();
        fs::write(dir.path().join("src/lib.rs"), "fn a() {}\n").unwrap();
        let cargo = FakeCargo::new();

        let result = tools(&dir, &cargo).prepare_edit_file(&edit("fn z", "fn c"));

        assert!(
            matches!(result, Err(ToolError::EditNotUnique { count: 0, .. })),
            "{result:?}"
        );
        assert_eq!(read(&dir, "src/lib.rs"), "fn a() {}\n");
    }

    #[test]
    fn edit_with_two_matches_fails_and_leaves_the_file_unchanged() {
        let dir = project();
        fs::write(dir.path().join("src/lib.rs"), "fn a() {}\nfn a() {}\n").unwrap();
        let cargo = FakeCargo::new();

        let result = tools(&dir, &cargo).prepare_edit_file(&edit("fn a", "fn c"));

        assert!(
            matches!(result, Err(ToolError::EditNotUnique { count: 2, .. })),
            "{result:?}"
        );
        assert_eq!(read(&dir, "src/lib.rs"), "fn a() {}\nfn a() {}\n");
    }

    #[test]
    fn edit_with_overlapping_matches_fails() {
        let dir = project();
        fs::write(dir.path().join("src/lib.rs"), "aaa").unwrap();
        let cargo = FakeCargo::new();

        let result = tools(&dir, &cargo).prepare_edit_file(&edit("aa", "b"));

        assert!(
            matches!(result, Err(ToolError::EditNotUnique { count: 2, .. })),
            "{result:?}"
        );
        assert_eq!(read(&dir, "src/lib.rs"), "aaa");
    }

    #[test]
    fn edit_with_an_empty_old_string_is_rejected() {
        let dir = project();
        fs::write(dir.path().join("src/lib.rs"), "").unwrap();
        let cargo = FakeCargo::new();

        let result = tools(&dir, &cargo).prepare_edit_file(&edit("", "fn a() {}\n"));

        assert!(
            matches!(&result, Err(ToolError::InvalidArguments { tool, .. }) if tool == "edit_file"),
            "{result:?}"
        );
    }

    #[test]
    fn edit_of_a_file_that_does_not_exist_fails() {
        let dir = project();
        let cargo = FakeCargo::new();

        let result = tools(&dir, &cargo).prepare_edit_file(&edit("fn a", "fn b"));

        assert!(
            matches!(&result, Err(ToolError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound),
            "{result:?}"
        );
    }

    #[test]
    fn edit_error_tells_the_model_how_to_retry() {
        let none = ToolError::EditNotUnique {
            path: "src/lib.rs".to_owned(),
            count: 0,
        };
        let many = ToolError::EditNotUnique {
            path: "src/lib.rs".to_owned(),
            count: 3,
        };

        assert_eq!(
            none.to_string(),
            "old_string does not occur in src/lib.rs: read the file and copy the text exactly"
        );
        assert_eq!(
            many.to_string(),
            "old_string occurs 3 times in src/lib.rs: include more of the surrounding text so it occurs exactly once"
        );
    }
}
