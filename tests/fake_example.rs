//! The example of `--fake` in `examples/fake/` takes one pass, used as a user is told to use it:
//! the script and the profile copied out of the checkout, since ① rewrites the profile.
//!
//! The user's side is `tests/fixtures/fake_pass/ui.toml`, the inputs README.md lists under "Try it
//! without a model", less the staging question: the project here has nothing uncommitted.
//! What each step of the pass shows and logs is for `tests/fake_pass.rs`; this checks that the
//! example's script and profile go together from the request to the commit.

mod common;

use std::fs;

use tempfile::TempDir;

use common::{assert_in_order, commit_count, git, in_repository, molt, molt_home, project};

/// The package name of the project in the tempdir.
const CRATE: &str = "molt_fake_example_probe";

#[test]
fn the_example_script_and_profile_copied_out_take_one_pass_to_an_approved_commit() {
    let dir = TempDir::new().unwrap();
    let project = project(dir.path(), CRATE);
    let home = molt_home(dir.path());
    let example = dir.path().join("example");
    fs::create_dir(&example).unwrap();
    for name in ["one_pass.toml", "profile.toml"] {
        fs::copy(
            in_repository(&format!("examples/fake/{name}")),
            example.join(name),
        )
        .unwrap();
    }
    let before = commit_count(&project);

    let output = molt(&project, &home)
        .arg("--fake")
        .arg(example.join("one_pass.toml"))
        .arg("--profile")
        .arg(example.join("profile.toml"))
        .arg("--ui-script")
        .arg(in_repository("tests/fixtures/fake_pass/ui.toml"))
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // The example profile does not allow `dbg!`, so the pass goes through the boundary and ①.
    // The message is the script's last response: without it molt would propose one of its own,
    // and a script that ran out early would still end in a commit.
    assert_in_order(
        &stdout,
        &[
            "Stopped at a boundary: the request needs dbg_macro (macros)",
            "── side line: dbg_macro ────",
            "Your profile allows dbg_macro now (known).",
            "cargo check passed.",
            "Message:\n  Print 1 with dbg!",
            "Commit with this message? [y/n] yes",
            "Committed.",
        ],
    );
    assert_eq!(commit_count(&project), before + 1);
    assert_eq!(git(&project, &["status", "--porcelain"]), "");
    // ① saved to the copy, which is why the example is copied before it is used.
    let saved = fs::read_to_string(example.join("profile.toml")).unwrap();
    assert!(
        saved.contains("[items.dbg_macro]\nstatus = \"known\"\nprovenance = \"side_line\""),
        "{saved}"
    );
}
