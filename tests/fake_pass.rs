//! One pass through molt without a model (PRD §10, the end of the dependency graph): a request →
//! a rejection → a boundary → the side line → ① → a write → `cargo check` → an approved commit.
//!
//! The binary runs as a user runs it, with the model's side from `--fake` and the user's side
//! from `--ui-script` (`tests/fixtures/fake_pass/`). `cargo` and `git` are the real ones, on a
//! cargo project with a git repository in a tempdir, so nothing here needs the network.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::cargo::cargo_bin_cmd;
use tempfile::TempDir;

/// The package name of the project in the tempdir. A name of its own, so its build does not
/// meet another test's crate of the same name.
const CRATE: &str = "molt_fake_pass_probe";

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

/// The stdout of `git <args>` in `dir`, which must succeed.
fn git(dir: &Path, args: &[&str]) -> String {
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

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/fake_pass")
        .join(name)
}

/// A cargo project in `dir` with `fn main() {}`, its `Cargo.lock`, and one commit of them all.
fn project(dir: &Path) -> PathBuf {
    let project = dir.join("project");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("Cargo.toml"),
        format!("[package]\nname = \"{CRATE}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n"),
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

/// Fails unless each of `expected` is found in `stdout`, in order.
fn assert_in_order(stdout: &str, expected: &[&str]) {
    let mut rest = stdout;
    for line in expected {
        let at = rest
            .find(line)
            .unwrap_or_else(|| panic!("{line:?} not found in order in:\n{stdout}"));
        rest = &rest[at + line.len()..];
    }
}

/// The `event` of each line of the session logs in `sessions`, with `boundary` for a rejection
/// and `status` for `cargo check`, in order.
fn logged_events(sessions: &Path) -> Vec<String> {
    let mut logs = fs::read_dir(sessions)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    logs.sort();
    logs.iter()
        .flat_map(|log| {
            fs::read_to_string(log)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
                .collect::<Vec<_>>()
        })
        .map(|event| match event["event"].as_str().unwrap() {
            "rejected" => format!("rejected (boundary: {})", event["boundary"]),
            "cargo_check" => format!("cargo_check ({})", event["status"].as_str().unwrap()),
            "tool_call" => format!("tool_call ({})", event["name"].as_str().unwrap()),
            other => other.to_owned(),
        })
        .collect()
}

#[test]
fn one_pass_from_a_request_through_a_boundary_and_unlock_ends_in_one_approved_commit() {
    let dir = TempDir::new().unwrap();
    let project = project(dir.path());
    let home = dir.path().join("molt-home");
    fs::create_dir(&home).unwrap();
    // No model can be reached: the key's variable is unset, so only the script answers.
    fs::write(
        home.join("config.toml"),
        "[api]\nbase_url = \"http://127.0.0.1:9/v1\"\napi_key_env = \"MOLT_TEST_FAKE_PASS_API_KEY\"\n",
    )
    .unwrap();
    let profile = dir.path().join("profile.toml");
    fs::write(&profile, "").unwrap();
    let before = git(&project, &["rev-list", "--count", "HEAD"]);

    let mut molt = cargo_bin_cmd!("molt");
    for (key, value) in GIT_ENV {
        match value {
            Some(value) => molt.env(key, value),
            None => molt.env_remove(key),
        };
    }
    let output = molt
        .current_dir(&project)
        .env("MOLT_HOME", &home)
        .env_remove("MOLT_TEST_FAKE_PASS_API_KEY")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("HOME")
        .env_remove("MOLT_LOG")
        // The project has no dependencies, so the `cargo check` molt runs needs no network;
        // this makes it fail instead of reaching out if that ever changes.
        .env("CARGO_NET_OFFLINE", "true")
        .arg("--profile")
        .arg(&profile)
        .arg("--ui-script")
        .arg(fixture("ui.toml"))
        .arg("--fake")
        .arg(fixture("fake.toml"))
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_in_order(
        &stdout,
        &[
            "molt> Print 1 to stderr with dbg! in src/main.rs.",
            "Stopped at a boundary: the request needs dbg_macro (macros)",
            "── side line: dbg_macro ────",
            "  `dbg!` prints the value of an expression",
            "molt [sub: dbg_macro]> /choose",
            "\n> 1\n",
            "── back to the main line ────",
            "Your profile allows dbg_macro now (known).",
            "Printed 1 with `dbg!`.",
            "+    dbg!(1);",
            "cargo check passed.",
            "To commit:",
            "Message:\n  Print 1 with dbg!",
            "Commit with this message? [y/n] yes",
            "Committed.",
            "molt> /quit",
        ],
    );
    // The first write was rejected and the rewrite was too, which made the boundary; the write
    // after ① passed, and molt's own `cargo check` at the end of the turn passed.
    let events = logged_events(&home.join("sessions"));
    let wanted = [
        "rejected (boundary: false)",
        "rejected (boundary: true)",
        "tool_call (write_file)",
        "tool_call (finish)",
        "cargo_check (passed)",
    ];
    let mut rest = events.iter();
    for event in wanted {
        assert!(
            rest.any(|logged| logged == event),
            "{event:?} not found in order in {events:#?}"
        );
    }

    // One commit more, with the approved message and molt's change to `src/main.rs`, and nothing
    // left over in the working tree. That a commit leaves out the user's other changes is for
    // the unit tests of `agent::mainline::commit`: there are none here.
    let after = git(&project, &["rev-list", "--count", "HEAD"]);
    assert_eq!(
        after.trim().parse::<u32>().unwrap(),
        before.trim().parse::<u32>().unwrap() + 1
    );
    assert_eq!(
        git(&project, &["log", "-1", "--format=%s"]),
        "Print 1 with dbg!\n"
    );
    assert_eq!(
        git(&project, &["show", "--name-only", "--format=", "HEAD"]),
        "src/main.rs\n"
    );
    assert_eq!(
        git(&project, &["show", "HEAD:src/main.rs"]),
        "fn main() {\n    dbg!(1);\n}\n"
    );
    assert_eq!(git(&project, &["status", "--porcelain"]), "");
    // ① made the element known, from the side line.
    let saved = fs::read_to_string(&profile).unwrap();
    assert!(
        saved.contains("[items.dbg_macro]\nstatus = \"known\"\nprovenance = \"side_line\""),
        "{saved}"
    );
}
