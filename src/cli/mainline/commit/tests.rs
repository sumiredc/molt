use std::fs;
use std::path::Path;
use std::process::ExitCode;

use serde_json::{Value, json};
use tempfile::TempDir;

use super::super::main_line;
use crate::agent::mainline::MainLine;
use crate::agent::tools::Tools;
use crate::config::Config;
use crate::llm::{ChatResponse, FakeChatClient, Router, ToolCall};
use crate::profile::store::Profile;
use crate::ui::{ScriptedUi, Step};
use crate::workspace::cargo::{CargoOutput, FakeCargo};
use crate::workspace::git::Git;
use crate::workspace::git::testing::{git_in, init, init_without_identity, without_repository};
use crate::workspace::path::Root;

const MAIN: &str = "fn main() {}\n";
const PRINTS: &str = "fn main() {\n    let _ = 1;\n}\n";

fn project() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"demo\"\n",
    )
    .unwrap();
    fs::create_dir(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/main.rs"), MAIN).unwrap();
    dir
}

/// A cargo project with everything in it committed as `first`, and a `Git` on it from `init`.
fn repository(init: fn(&Path) -> Git) -> (TempDir, Git) {
    let dir = project();
    let git = init(dir.path());
    for args in [&["add", "."][..], &["commit", "--quiet", "--message=first"]] {
        assert!(git_in(dir.path(), args).status().unwrap().success());
    }
    (dir, git)
}

fn log(dir: &Path) -> Vec<String> {
    let output = git_in(dir, &["log", "--format=%s"]).output().unwrap();
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn status(dir: &Path) -> String {
    let output = git_in(dir, &["status", "--porcelain=v1"]).output().unwrap();
    String::from_utf8(output.stdout).unwrap()
}

/// A cargo whose `cargo check`s give `passes` in turn.
fn cargo(passes: &[bool]) -> FakeCargo {
    let cargo = FakeCargo::new();
    for &success in passes {
        cargo.push(Ok(CargoOutput {
            success,
            stdout: String::new(),
            stderr: if success { "" } else { "error[E0308]" }.to_owned(),
        }));
    }
    cargo
}

/// Runs the main line on `steps` in `dir` with `git`, and returns what the user saw and was asked.
fn run(
    dir: &TempDir,
    git: Git,
    client: &FakeChatClient,
    cargo: &FakeCargo,
    steps: impl IntoIterator<Item = Step>,
) -> ScriptedUi {
    let tools = Tools::with_git(Root::new(dir.path()).unwrap(), cargo, git);
    let mut agent = MainLine::new(
        client,
        Router::new(&Config::default()),
        tools,
        Profile::new(),
    );
    let mut ui = ScriptedUi::new(steps);
    let code = main_line(&mut ui, &mut agent);
    assert_eq!(code, ExitCode::SUCCESS, "{:?}", ui.shown());
    ui
}

fn input(line: &str) -> Step {
    Step::Input(line.to_owned())
}

fn call(id: &str, name: &str, arguments: Value) -> ChatResponse {
    ChatResponse {
        content: None,
        tool_calls: vec![ToolCall {
            id: id.to_owned(),
            name: name.to_owned(),
            arguments: arguments.to_string(),
        }],
    }
}

fn reply(text: &str) -> ChatResponse {
    ChatResponse {
        content: Some(text.to_owned()),
        tool_calls: Vec::new(),
    }
}

/// The model writes `src/main.rs` and finishes.
fn push_write(client: &FakeChatClient) {
    client.push_chat(Ok(call(
        "write",
        "write_file",
        json!({ "path": "src/main.rs", "content": PRINTS }),
    )));
    client.push_chat(Ok(reply("Printed 1.")));
}

/// The model writes `src/main.rs`, calls `git_commit` and replies.
fn push_write_and_commit(client: &FakeChatClient) {
    client.push_chat(Ok(call(
        "write",
        "write_file",
        json!({ "path": "src/main.rs", "content": PRINTS }),
    )));
    client.push_chat(Ok(call(
        "commit",
        "git_commit",
        json!({ "message": "Print 1" }),
    )));
    client.push_chat(Ok(reply("Committed.")));
}

/// Whether one of `shown` contains `text`.
fn shows(shown: &[String], text: &str) -> bool {
    shown.iter().any(|line| line.contains(text))
}

#[test]
fn after_a_turn_that_wrote_molt_shows_the_diff_and_the_message_and_commits_on_yes() {
    let (dir, git) = repository(init);
    let client = FakeChatClient::new();
    push_write(&client);
    client.push_chat(Ok(reply("Print 1")));
    let cargo = cargo(&[true]);

    let ui = run(
        &dir,
        git,
        &client,
        &cargo,
        [input("print 1"), Step::Confirm(true), input("/quit")],
    );

    let shown = ui.shown();
    let proposal = shown.iter().position(|s| s == "To commit:").unwrap();
    assert!(
        shown[proposal + 1].contains("--- a/src/main.rs\n+++ b/src/main.rs\n"),
        "{shown:?}"
    );
    assert_eq!(shown[proposal + 2], "Message:\n  Print 1");
    assert_eq!(ui.asked()[1], "Commit with this message?");
    assert!(
        shown.last().unwrap().starts_with("Committed.\n["),
        "{shown:?}"
    );
    assert_eq!(log(dir.path()), ["Print 1", "first"]);
}

#[test]
fn a_no_to_the_proposal_commits_nothing_and_says_so() {
    let (dir, git) = repository(init);
    let client = FakeChatClient::new();
    push_write(&client);
    client.push_chat(Ok(reply("Print 1")));
    let cargo = cargo(&[true]);

    let ui = run(
        &dir,
        git,
        &client,
        &cargo,
        [input("print 1"), Step::Confirm(false), input("/quit")],
    );

    assert_eq!(ui.shown().last().unwrap(), "Not committed.");
    assert_eq!(log(dir.path()), ["first"]);
    assert_eq!(status(dir.path()), " M src/main.rs\n");
}

#[test]
fn other_changes_are_listed_and_asked_about_before_the_commit() {
    let (dir, git) = repository(init);
    fs::write(dir.path().join("notes.txt"), "mine\n").unwrap();
    let client = FakeChatClient::new();
    push_write(&client);
    client.push_chat(Ok(reply("Print 1")));
    let cargo = cargo(&[true]);

    let ui = run(
        &dir,
        git,
        &client,
        &cargo,
        [
            input("print 1"),
            Step::Confirm(true),
            Step::Confirm(true),
            input("/quit"),
        ],
    );

    assert!(
        ui.shown().contains(
            &"Other changes are not part of this commit and stay as they are:\n  notes.txt"
                .to_owned()
        ),
        "{:?}",
        ui.shown()
    );
    assert_eq!(
        ui.asked()[1..3],
        [
            "Stage and commit only the files molt changed?",
            "Commit with this message?"
        ]
    );
    assert_eq!(log(dir.path()), ["Print 1", "first"]);
    assert_eq!(status(dir.path()), "?? notes.txt\n");
}

#[test]
fn a_no_to_leaving_the_other_changes_out_commits_nothing() {
    let (dir, git) = repository(init);
    fs::write(dir.path().join("notes.txt"), "mine\n").unwrap();
    let client = FakeChatClient::new();
    push_write(&client);
    client.push_chat(Ok(reply("Print 1")));
    let cargo = cargo(&[true]);

    let ui = run(
        &dir,
        git,
        &client,
        &cargo,
        [input("print 1"), Step::Confirm(false), input("/quit")],
    );

    assert_eq!(ui.shown().last().unwrap(), "Not committed.");
    assert_eq!(log(dir.path()), ["first"]);
}

#[test]
fn with_the_last_check_failing_git_commit_warns_and_asks_before_anything_else() {
    let (dir, git) = repository(init);
    let client = FakeChatClient::new();
    push_write_and_commit(&client);
    // After the failed check at the end of the turn, the model gives up.
    client.push_chat(Ok(reply("I cannot fix it.")));
    let cargo = cargo(&[false, false]);

    let ui = run(
        &dir,
        git,
        &client,
        &cargo,
        [
            input("print 1 and commit"),
            Step::Confirm(false),
            input("/quit"),
        ],
    );

    assert!(
        shows(
            ui.shown(),
            "Warning: the last cargo check molt ran did not pass."
        ),
        "{:?}",
        ui.shown()
    );
    assert_eq!(ui.asked()[1..], ["Commit anyway?", "molt> "]);
    assert!(shows(ui.shown(), "Not committed."));
    assert_eq!(log(dir.path()), ["first"]);
}

#[test]
fn a_yes_despite_the_failing_check_commits() {
    let (dir, git) = repository(init);
    let client = FakeChatClient::new();
    push_write_and_commit(&client);
    client.push_chat(Ok(reply("I cannot fix it.")));
    let cargo = cargo(&[false, false]);

    let ui = run(
        &dir,
        git,
        &client,
        &cargo,
        [
            input("print 1 and commit"),
            Step::Confirm(true),
            Step::Confirm(true),
            input("/quit"),
        ],
    );

    assert_eq!(
        ui.asked()[1..3],
        ["Commit anyway?", "Commit with this message?"]
    );
    assert_eq!(log(dir.path()), ["Print 1", "first"]);
}

#[test]
fn a_commit_git_refuses_shows_the_error_and_that_the_files_stay_staged() {
    let (dir, git) = repository(init_without_identity);
    let client = FakeChatClient::new();
    push_write_and_commit(&client);
    let cargo = cargo(&[true, true]);

    let ui = run(
        &dir,
        git,
        &client,
        &cargo,
        [
            input("print 1 and commit"),
            Step::Confirm(true),
            input("/quit"),
        ],
    );

    let failed = ui
        .shown()
        .iter()
        .find(|s| s.starts_with("The commit failed"))
        .unwrap_or_else(|| panic!("{:?}", ui.shown()));
    assert!(
        failed.starts_with("The commit failed, so nothing was committed. The files stay staged.\n"),
        "{failed}"
    );
    assert!(failed.contains("user.name"), "{failed}");
    assert_eq!(log(dir.path()), ["first"]);
    assert_eq!(status(dir.path()), "M  src/main.rs\n");
}

/// A [`ScriptedUi`] whose user writes `src/main.rs` in their editor right before `/done`.
struct WritesOnDone {
    ui: ScriptedUi,
    path: std::path::PathBuf,
    content: &'static str,
}

impl crate::ui::Ui for WritesOnDone {
    fn read_line(&mut self, prompt: &str) -> Result<crate::ui::Input, crate::ui::UiError> {
        let input = self.ui.read_line(prompt)?;
        if matches!(&input, crate::ui::Input::Line(line) if line == "/done") {
            fs::write(&self.path, self.content).unwrap();
        }
        Ok(input)
    }

    fn show(&mut self, text: &str) {
        self.ui.show(text);
    }

    fn select(
        &mut self,
        prompt: &str,
        items: &[&str],
    ) -> Result<Option<usize>, crate::ui::UiError> {
        self.ui.select(prompt, items)
    }

    fn confirm(&mut self, prompt: &str) -> Result<Option<bool>, crate::ui::UiError> {
        self.ui.confirm(prompt)
    }

    fn form(
        &mut self,
        form: &crate::ui::Form,
        initial: Option<&crate::ui::FormValues>,
    ) -> Result<Option<crate::ui::FormValues>, crate::ui::UiError> {
        self.ui.form(form, initial)
    }
}

#[test]
fn after_the_user_wrote_it_in_3_molt_proposes_the_commit_once_the_turn_goes_on_and_ends() {
    const WITH_DBG: &str = "fn main() {\n    dbg!(1);\n}\n";
    let (dir, git) = repository(init);
    let client = FakeChatClient::new();
    for id in ["first", "second"] {
        client.push_chat(Ok(call(
            id,
            "write_file",
            json!({ "path": "src/main.rs", "content": WITH_DBG }),
        )));
    }
    for text in [
        "explanation",
        "Write dbg!(1) in main.",
        "Good use of dbg!.",
        "The request is done.",
        "Debug 1",
    ] {
        client.push_chat(Ok(reply(text)));
    }
    let cargo = cargo(&[true]);
    let tools = Tools::with_git(Root::new(dir.path()).unwrap(), &cargo, git);
    let mut agent = MainLine::new(
        &client,
        Router::new(&Config::default()),
        tools,
        Profile::new(),
    );
    let mut ui = WritesOnDone {
        ui: ScriptedUi::new([
            input("debug 1"),
            input("/choose"),
            Step::Select(3),
            input("/done"),
            Step::Confirm(true),
            input("/quit"),
        ]),
        path: dir.path().join("src/main.rs"),
        content: WITH_DBG,
    };

    let code = main_line(&mut ui, &mut agent);

    assert_eq!(code, ExitCode::SUCCESS, "{:?}", ui.ui.shown());
    assert!(shows(ui.ui.shown(), "To commit:"), "{:?}", ui.ui.shown());
    assert_eq!(log(dir.path()), ["Debug 1", "first"]);
}

#[test]
fn git_commit_outside_a_git_repository_says_so_and_asks_nothing() {
    let dir = project();
    let git = without_repository(dir.path());
    let client = FakeChatClient::new();
    push_write_and_commit(&client);
    let cargo = cargo(&[true, true]);

    let ui = run(
        &dir,
        git,
        &client,
        &cargo,
        [input("print 1 and commit"), input("/quit")],
    );

    assert!(
        shows(ui.shown(), "Not committed: this is not a git repository."),
        "{:?}",
        ui.shown()
    );
    assert_eq!(ui.asked(), ["molt> "; 2]);
}

#[test]
fn git_commit_with_nothing_to_commit_says_so_and_asks_nothing() {
    let (dir, git) = repository(init);
    let client = FakeChatClient::new();
    client.push_chat(Ok(call(
        "commit",
        "git_commit",
        json!({ "message": "Nothing" }),
    )));
    client.push_chat(Ok(reply("Nothing to commit.")));
    let cargo = FakeCargo::new();

    let ui = run(
        &dir,
        git,
        &client,
        &cargo,
        [input("commit"), input("/quit")],
    );

    assert!(
        shows(
            ui.shown(),
            "Nothing to commit: the files molt changed are as git has them."
        ),
        "{:?}",
        ui.shown()
    );
    assert_eq!(ui.asked(), ["molt> "; 2]);
}
