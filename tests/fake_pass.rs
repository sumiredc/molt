//! One pass through molt without a model (PRD §10, the end of the dependency graph): a request →
//! a rejection → a boundary → the side line → ① → a write → `cargo check` → an approved commit.
//!
//! The binary runs as a user runs it, with the model's side from `--fake` and the user's side
//! from `--ui-script` (`examples/fake/one_pass.toml` and `tests/fixtures/fake_pass/ui.toml`).
//! `cargo` and `git` are the real ones, on a cargo project with a git repository in a tempdir, so
//! nothing here needs the network.

mod common;

use std::fs;
use std::path::Path;

use tempfile::TempDir;

use common::{assert_in_order, commit_count, git, in_repository, molt, molt_home, project};

/// The package name of the project in the tempdir.
const CRATE: &str = "molt_fake_pass_probe";

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
    let project = project(dir.path(), CRATE);
    let home = molt_home(dir.path());
    let profile = dir.path().join("profile.toml");
    fs::write(&profile, "").unwrap();
    let before = commit_count(&project);

    let output = molt(&project, &home)
        .arg("--profile")
        .arg(&profile)
        .arg("--ui-script")
        .arg(in_repository("tests/fixtures/fake_pass/ui.toml"))
        .arg("--fake")
        .arg(in_repository("examples/fake/one_pass.toml"))
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
    assert_eq!(commit_count(&project), before + 1);
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
