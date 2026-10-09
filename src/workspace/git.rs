//! Runs the four git operations the agent may use, status, diff, add and commit, with a fixed
//! argv (PRD §8.1, §11). There is no way to push from here (PRD §3).

use std::cell::RefCell;
use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};

use super::Approval;
use super::path::{PathError, Root};

/// Which changes [`Git::diff`] shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffScope {
    /// The working tree against the index: what `add` would stage.
    Unstaged,
    /// The index against `HEAD`: what `commit` would record.
    Staged,
}

/// What git printed and whether it succeeded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Why git could not be run. A git that ran and failed is a [`GitOutput`] instead.
#[derive(Debug)]
pub enum GitError {
    /// A path given to [`Git::add`] is not under the project root.
    Path(PathError),
    /// The `git` process could not be started.
    Spawn(io::Error),
}

/// Runs git in the project root and records every argv it runs.
#[derive(Debug)]
pub struct Git {
    root: Root,
    /// Environment variables to set (`Some`) or remove (`None`) for git. Empty outside tests:
    /// the author and the rest of the config come from the user's own git config.
    envs: Vec<(&'static str, Option<&'static str>)>,
    invocations: RefCell<Vec<Vec<OsString>>>,
}

impl Git {
    pub fn new(root: Root) -> Self {
        Self {
            root,
            envs: Vec::new(),
            invocations: RefCell::new(Vec::new()),
        }
    }

    /// `git status --porcelain=v1`: a format the user's git config does not change.
    pub fn status(&self) -> Result<GitOutput, GitError> {
        self.execute(argv(&["status", "--porcelain=v1"]))
    }

    /// `git diff`, or `git diff --cached` for [`DiffScope::Staged`], without colour or an
    /// external diff program whatever the user's git config says.
    pub fn diff(&self, scope: DiffScope) -> Result<GitOutput, GitError> {
        let argv = match scope {
            DiffScope::Unstaged => argv(&["diff", "--no-color", "--no-ext-diff"]),
            DiffScope::Staged => argv(&["diff", "--no-color", "--no-ext-diff", "--cached"]),
        };
        self.execute(argv)
    }

    /// `git add -- <paths>`. Every path must resolve under the project root; if one does not,
    /// git is not run. A symlink is resolved first, so its target is staged, the file molt
    /// writes through it.
    pub fn add(&self, paths: &[impl AsRef<Path>]) -> Result<GitOutput, GitError> {
        let mut argv = argv(&["add", "--"]);
        for path in paths {
            let resolved = self.root.resolve_read(path).map_err(GitError::Path)?;
            argv.push(resolved.into_os_string());
        }
        self.execute(argv)
    }

    /// `git commit --message=<message>`: records what is staged. A commit needs the user's
    /// approval (PRD §3, §8.4):
    ///
    /// ```compile_fail
    /// use molt::workspace::git::Git;
    /// use molt::workspace::path::Root;
    ///
    /// let git = Git::new(Root::new(".").unwrap());
    /// git.commit("message");
    /// ```
    pub fn commit(&self, message: &str, _approval: Approval) -> Result<GitOutput, GitError> {
        let mut message_arg = OsString::from("--message=");
        message_arg.push(message);
        self.execute(vec![OsString::from("commit"), message_arg])
    }

    /// Every argv run so far, in order, without the leading `git`.
    pub fn invocations(&self) -> Vec<Vec<OsString>> {
        self.invocations.borrow().clone()
    }

    /// Records `argv`, then runs `git` with it in the project root.
    fn execute(&self, argv: Vec<OsString>) -> Result<GitOutput, GitError> {
        self.invocations.borrow_mut().push(argv.clone());
        let mut command = Command::new("git");
        for (key, value) in &self.envs {
            match value {
                Some(value) => command.env(key, value),
                None => command.env_remove(key),
            };
        }
        let output = command
            .args(&argv)
            .current_dir(self.root.as_path())
            .stdin(Stdio::null())
            .output()
            .map_err(GitError::Spawn)?;
        Ok(GitOutput {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

fn argv(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

impl fmt::Display for GitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Path(source) => write!(f, "cannot stage: {source}"),
            Self::Spawn(source) => write!(f, "cannot run git: {source}"),
        }
    }
}

impl Error for GitError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Path(source) => Some(source),
            Self::Spawn(source) => Some(source),
        }
    }
}

/// Runs a real `git` in a tempdir for the tests here and in `agent::tools`.
#[cfg(test)]
pub(crate) mod testing {
    use std::path::Path;
    use std::process::Command;

    use super::Git;
    use crate::workspace::path::Root;

    /// Keeps the developer's own git config out of the tests, and gives commits an author by
    /// environment instead of by config, so `Git` itself never sets one. Removes the variables
    /// that point git at another repository, which a git hook running the tests would set.
    const TEST_ENV: [(&str, Option<&str>); 11] = [
        ("GIT_CONFIG_GLOBAL", Some("/dev/null")),
        ("GIT_CONFIG_NOSYSTEM", Some("1")),
        ("GIT_AUTHOR_NAME", Some("molt test")),
        ("GIT_AUTHOR_EMAIL", Some("molt-test@example.com")),
        ("GIT_COMMITTER_NAME", Some("molt test")),
        ("GIT_COMMITTER_EMAIL", Some("molt-test@example.com")),
        ("GIT_DIR", None),
        ("GIT_WORK_TREE", None),
        ("GIT_INDEX_FILE", None),
        ("GIT_OBJECT_DIRECTORY", None),
        ("GIT_COMMON_DIR", None),
    ];

    /// `git <args>` in `dir`, with [`TEST_ENV`].
    pub(crate) fn git_in(dir: &Path, args: &[&str]) -> Command {
        let mut command = Command::new("git");
        for (key, value) in TEST_ENV {
            match value {
                Some(value) => command.env(key, value),
                None => command.env_remove(key),
            };
        }
        command.args(args).current_dir(dir);
        command
    }

    /// Runs `git init` in `dir` and returns a `Git` on it that runs with [`TEST_ENV`].
    pub(crate) fn init(dir: &Path) -> Git {
        let status = git_in(dir, &["init", "--quiet"]).status().unwrap();
        assert!(status.success());
        let mut git = Git::new(Root::new(dir).unwrap());
        git.envs = TEST_ENV.to_vec();
        git
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::testing::{git_in, init};
    use super::*;
    use crate::workspace::approve;

    /// An empty git repository in a tempdir, and a `Git` on it.
    fn repository() -> (tempfile::TempDir, Git) {
        let dir = tempfile::tempdir().unwrap();
        let git = init(dir.path());
        (dir, git)
    }

    /// A repository with `tracked.txt` committed.
    fn repository_with_a_commit() -> (tempfile::TempDir, Git) {
        let (dir, git) = repository();
        fs::write(dir.path().join("tracked.txt"), "old\n").unwrap();
        for argv in [
            &["add", "tracked.txt"][..],
            &["commit", "--quiet", "--message=first"],
        ] {
            let status = git_in(dir.path(), argv).status().unwrap();
            assert!(status.success());
        }
        (dir, git)
    }

    /// The subjects of the commits on `HEAD`, newest first.
    fn log(dir: &Path) -> Vec<String> {
        let output = git_in(dir, &["log", "--format=%s"]).output().unwrap();
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// The paths in the last commit.
    fn last_commit_files(dir: &Path) -> Vec<String> {
        let output = git_in(dir, &["show", "--format=", "--name-only", "HEAD"])
            .output()
            .unwrap();
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn status_lists_an_untracked_file() {
        let (dir, git) = repository();
        fs::write(dir.path().join("new.txt"), "").unwrap();

        let output = git.status().unwrap();

        assert!(output.success, "{}", output.stderr);
        assert_eq!(output.stdout, "?? new.txt\n");
    }

    #[test]
    fn unstaged_diff_shows_a_change_in_the_working_tree() {
        let (dir, git) = repository_with_a_commit();
        fs::write(dir.path().join("tracked.txt"), "new\n").unwrap();

        let output = git.diff(DiffScope::Unstaged).unwrap();

        assert!(output.success, "{}", output.stderr);
        assert!(output.stdout.contains("-old\n+new\n"), "{}", output.stdout);
    }

    #[test]
    fn staged_diff_shows_only_what_is_staged() {
        let (dir, git) = repository_with_a_commit();
        fs::write(dir.path().join("tracked.txt"), "new\n").unwrap();
        fs::write(dir.path().join("staged.txt"), "staged\n").unwrap();
        git.add(&["staged.txt"]).unwrap();

        let output = git.diff(DiffScope::Staged).unwrap();

        assert!(output.success, "{}", output.stderr);
        assert!(output.stdout.contains("+staged\n"), "{}", output.stdout);
        assert!(!output.stdout.contains("tracked.txt"), "{}", output.stdout);
    }

    #[test]
    fn add_stages_only_the_given_paths() {
        let (dir, git) = repository();
        fs::write(dir.path().join("a.txt"), "").unwrap();
        fs::write(dir.path().join("b.txt"), "").unwrap();

        let output = git.add(&["a.txt"]).unwrap();

        assert!(output.success, "{}", output.stderr);
        assert_eq!(git.status().unwrap().stdout, "A  a.txt\n?? b.txt\n");
    }

    #[test]
    fn add_stages_a_deleted_file() {
        let (dir, git) = repository_with_a_commit();
        fs::remove_file(dir.path().join("tracked.txt")).unwrap();

        let output = git.add(&["tracked.txt"]).unwrap();

        assert!(output.success, "{}", output.stderr);
        assert_eq!(git.status().unwrap().stdout, "D  tracked.txt\n");
    }

    #[test]
    fn add_with_a_path_outside_the_root_runs_no_git() {
        let (dir, git) = repository();
        fs::write(dir.path().join("a.txt"), "").unwrap();

        let result = git.add(&["a.txt", "../outside.txt"]);

        assert!(
            matches!(result, Err(GitError::Path(PathError::OutsideRoot { .. }))),
            "{result:?}"
        );
        assert_eq!(git.invocations(), Vec::<Vec<OsString>>::new());
    }

    #[test]
    fn commit_with_approval_records_only_what_is_staged() {
        let (dir, git) = repository_with_a_commit();
        fs::write(dir.path().join("tracked.txt"), "new\n").unwrap();
        fs::write(dir.path().join("staged.txt"), "").unwrap();
        git.add(&["staged.txt"]).unwrap();

        let output = git.commit("Add staged.txt", approve()).unwrap();

        assert!(output.success, "{}", output.stderr);
        assert_eq!(log(dir.path()), ["Add staged.txt", "first"]);
        assert_eq!(last_commit_files(dir.path()), ["staged.txt"]);
    }

    #[test]
    fn commit_with_nothing_staged_fails_without_a_new_commit() {
        let (dir, git) = repository_with_a_commit();

        let output = git.commit("Nothing", approve()).unwrap();

        assert!(!output.success);
        assert_eq!(log(dir.path()), ["first"]);
    }

    #[test]
    fn commit_message_starting_with_a_dash_is_not_read_as_an_option() {
        let (dir, git) = repository_with_a_commit();
        fs::write(dir.path().join("a.txt"), "").unwrap();
        git.add(&["a.txt"]).unwrap();

        let output = git.commit("--amend", approve()).unwrap();

        assert!(output.success, "{}", output.stderr);
        assert_eq!(log(dir.path()), ["--amend", "first"]);
    }

    #[test]
    fn each_operation_runs_its_fixed_argv_and_none_pushes() {
        let (dir, git) = repository();
        fs::write(dir.path().join("a.txt"), "a\n").unwrap();

        let outputs = [
            git.status().unwrap(),
            git.diff(DiffScope::Unstaged).unwrap(),
            git.add(&["a.txt"]).unwrap(),
            git.diff(DiffScope::Staged).unwrap(),
            git.commit("Add a.txt", approve()).unwrap(),
        ];

        for output in &outputs {
            assert!(output.success, "{}", output.stderr);
        }
        assert_eq!(log(dir.path()), ["Add a.txt"]);

        let a_txt = dir.path().canonicalize().unwrap().join("a.txt");
        let mut add = argv(&["add", "--"]);
        add.push(a_txt.into_os_string());
        let invocations = git.invocations();
        assert_eq!(
            invocations,
            [
                argv(&["status", "--porcelain=v1"]),
                argv(&["diff", "--no-color", "--no-ext-diff"]),
                add,
                argv(&["diff", "--no-color", "--no-ext-diff", "--cached"]),
                argv(&["commit", "--message=Add a.txt"]),
            ]
        );
        // Only the four operations reach git, so no argv pushes.
        assert!(
            invocations.iter().all(
                |argv| ["status", "diff", "add", "commit"].contains(&argv[0].to_str().unwrap())
            ),
            "{invocations:?}"
        );
    }
}
