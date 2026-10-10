//! What the tests that run one pass of the `molt` binary share: a cargo project with a git
//! repository in a tempdir, a molt directory that no model can be reached from, and the binary
//! set up to run there with the real `cargo` and `git` and no network.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::cargo::cargo_bin;

/// The variable `config.toml` reads the API key from. It is left unset, so only a `--fake`
/// script can answer.
const API_KEY_ENV: &str = "MOLT_TEST_FAKE_PASS_API_KEY";

/// Keeps the developer's own git config out, and gives commits an author by environment, for the
/// git run here and the one molt runs. Removes the variables that point git at another
/// repository, which a git hook running the tests would set.
const GIT_ENV: [(&str, Option<&str>); 11] = [
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

fn with_git_env(command: &mut Command) -> &mut Command {
    for (key, value) in GIT_ENV {
        match value {
            Some(value) => command.env(key, value),
            None => command.env_remove(key),
        };
    }
    command
}

/// A file of this repository, by its path from the repository root.
pub fn in_repository(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(path)
}

/// The stdout of `git <args>` in `dir`, which must succeed.
pub fn git(dir: &Path, args: &[&str]) -> String {
    let output = with_git_env(&mut Command::new("git"))
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

/// The number of commits on `HEAD` in `project`.
pub fn commit_count(project: &Path) -> u32 {
    git(project, &["rev-list", "--count", "HEAD"])
        .trim()
        .parse()
        .unwrap()
}

/// A cargo project named `crate_name` in `dir` with `fn main() {}`, its `Cargo.lock`, and one
/// commit of them all. Give each test a name of its own, so its build does not meet another
/// test's crate of the same name.
pub fn project(dir: &Path, crate_name: &str) -> PathBuf {
    let project = dir.join("project");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("Cargo.toml"),
        format!("[package]\nname = \"{crate_name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n"),
    )
    .unwrap();
    fs::write(project.join("src/main.rs"), "fn main() {}\n").unwrap();
    fs::write(project.join(".gitignore"), "/target\n").unwrap();
    // `cargo check` writes `Cargo.lock` when it is missing, which would be a change of the user's.
    let lock = Command::new("cargo")
        .args(["generate-lockfile", "--offline", "--quiet"])
        .current_dir(&project)
        .output()
        .unwrap();
    assert!(
        lock.status.success(),
        "{}",
        String::from_utf8_lossy(&lock.stderr)
    );
    git(&project, &["init", "--quiet"]);
    git(&project, &["add", "--", "."]);
    git(&project, &["commit", "--quiet", "--message=init"]);
    project
}

/// A molt directory in `dir` whose `config.toml` points at no reachable model.
pub fn molt_home(dir: &Path) -> PathBuf {
    let home = dir.join("molt-home");
    fs::create_dir(&home).unwrap();
    fs::write(
        home.join("config.toml"),
        format!("[api]\nbase_url = \"http://127.0.0.1:9/v1\"\napi_key_env = \"{API_KEY_ENV}\"\n"),
    )
    .unwrap();
    home
}

/// `molt` to run in `project` with `home` as its molt directory, the git environment above, and
/// none of the developer's own molt settings. The arguments are the caller's.
pub fn molt(project: &Path, home: &Path) -> Command {
    let mut molt = Command::new(cargo_bin!("molt"));
    with_git_env(&mut molt)
        .current_dir(project)
        .env("MOLT_HOME", home)
        .env_remove(API_KEY_ENV)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("HOME")
        .env_remove("MOLT_LOG")
        // The project has no dependencies, so the `cargo check` molt runs needs no network;
        // this makes it fail instead of reaching out if that ever changes.
        .env("CARGO_NET_OFFLINE", "true");
    molt
}

/// Fails unless each of `expected` is found in `stdout`, in order.
pub fn assert_in_order(stdout: &str, expected: &[&str]) {
    let mut rest = stdout;
    for line in expected {
        let at = rest
            .find(line)
            .unwrap_or_else(|| panic!("{line:?} not found in order in:\n{stdout}"));
        rest = &rest[at + line.len()..];
    }
}
