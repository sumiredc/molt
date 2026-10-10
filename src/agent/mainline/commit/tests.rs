//! Tests of the commit of the files molt changed (PRD UC4), in a git repository in a tempdir.

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tempfile::TempDir;

use super::*;
use crate::agent::subline::{Choice, Done};
use crate::agent::tools::Tools;
use crate::config::Config;
use crate::llm::{ChatError, ChatResponse, FakeChatClient, Router, ToolCall};
use crate::profile::store::Profile;
use crate::session::SessionLog;
use crate::workspace::Approval;
use crate::workspace::cargo::{
    CargoCall, CargoError, CargoOutput, CargoRunner, CrateName, FakeCargo, Subcommand,
};
use crate::workspace::git::Git;
use crate::workspace::git::testing::{git_in, init, init_without_identity, without_repository};
use crate::workspace::path::Root;

/// Answers every approval with `answer`, and records what was asked.
struct Answer {
    answer: bool,
    asked: Vec<ApprovalRequest>,
}

impl Answer {
    fn yes() -> Self {
        Self {
            answer: true,
            asked: Vec::new(),
        }
    }

    fn no() -> Self {
        Self {
            answer: false,
            asked: Vec::new(),
        }
    }

    /// The commits the user was asked to approve.
    fn proposals(&self) -> Vec<CommitProposal> {
        self.asked
            .iter()
            .filter_map(|request| match request {
                ApprovalRequest::GitCommit(proposal) => Some(proposal.clone()),
                ApprovalRequest::CargoAdd(_) => None,
            })
            .collect()
    }
}

impl Approver for Answer {
    fn confirm(&mut self, request: &ApprovalRequest) -> bool {
        self.asked.push(request.clone());
        self.answer
    }
}

const MANIFEST: &str = "[package]\nname = \"demo\"\n\n[dependencies]\n";
const LOCK: &str = "version = 4\n";
const MAIN: &str = "fn main() {}\n";
const PRINTS: &str = "fn main() {\n    println!(\"{}\", 1);\n}\n";
const PRINTS_2: &str = "fn main() {\n    println!(\"{}\", 2);\n}\n";
const WITH_DBG: &str = "fn main() {\n    dbg!(1);\n}\n";

/// A cargo project in a tempdir.
fn project() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("Cargo.toml"), MANIFEST).unwrap();
    fs::write(dir.path().join("Cargo.lock"), LOCK).unwrap();
    fs::create_dir(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/main.rs"), MAIN).unwrap();
    dir
}

/// A cargo project with everything in it committed as `first`, and a `Git` on it.
fn repository() -> (TempDir, Git) {
    repository_with(init)
}

fn repository_with(init: fn(&Path) -> Git) -> (TempDir, Git) {
    let dir = project();
    let git = init(dir.path());
    run_git(dir.path(), &["add", "."]);
    run_git(dir.path(), &["commit", "--quiet", "--message=first"]);
    (dir, git)
}

fn run_git(dir: &Path, args: &[&str]) {
    let status = git_in(dir, args).status().unwrap();
    assert!(status.success(), "git {args:?}");
}

fn git_output(dir: &Path, args: &[&str]) -> String {
    let output = git_in(dir, args).output().unwrap();
    String::from_utf8(output.stdout).unwrap()
}

/// The subjects of the commits on `HEAD`, newest first.
fn log(dir: &Path) -> Vec<String> {
    git_output(dir, &["log", "--format=%s"])
        .lines()
        .map(str::to_owned)
        .collect()
}

/// The paths in the last commit, sorted.
fn last_commit_files(dir: &Path) -> Vec<String> {
    let mut files: Vec<String> = git_output(dir, &["show", "--format=", "--name-only", "HEAD"])
        .lines()
        .map(str::to_owned)
        .collect();
    files.sort();
    files
}

fn status(dir: &Path) -> String {
    git_output(dir, &["status", "--porcelain=v1", "--untracked-files=all"])
}

fn main_line<'a>(
    client: &'a FakeChatClient,
    cargo: &'a dyn CargoRunner,
    dir: &TempDir,
    git: Git,
) -> MainLine<'a> {
    let tools = Tools::with_git(Root::new(dir.path()).unwrap(), cargo, git);
    MainLine::new(
        client,
        Router::new(&Config::default()),
        tools,
        Profile::new(),
    )
}

/// The subcommands of every git run so far.
fn git_subcommands(main_line: &MainLine) -> Vec<String> {
    main_line
        .tools
        .git()
        .invocations()
        .iter()
        .map(|argv| argv[0].to_str().unwrap().to_owned())
        .collect()
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

fn write(id: &str, path: &str, content: &str) -> ChatResponse {
    call(
        id,
        "write_file",
        json!({ "path": path, "content": content }),
    )
}

fn git_commit(id: &str, message: &str) -> ChatResponse {
    call(id, "git_commit", json!({ "message": message }))
}

fn reply(text: &str) -> ChatResponse {
    ChatResponse {
        content: Some(text.to_owned()),
        tool_calls: Vec::new(),
    }
}

fn output(success: bool) -> Result<CargoOutput, CargoError> {
    Ok(CargoOutput {
        success,
        stdout: String::new(),
        stderr: if success { "" } else { "error[E0308]" }.to_owned(),
    })
}

/// A cargo whose `cargo check`s give `passes` in turn.
fn cargo(passes: &[bool]) -> FakeCargo {
    let cargo = FakeCargo::new();
    for &passed in passes {
        cargo.push(output(passed));
    }
    cargo
}

/// Runs a turn on `input` in which the model writes `content` to `path` and then replies.
fn write_turn(
    client: &FakeChatClient,
    main_line: &mut MainLine,
    path: &str,
    content: &str,
    approver: &mut dyn Approver,
) -> Turn {
    client.push_chat(Ok(write("write", path, content)));
    client.push_chat(Ok(reply("done")));
    main_line.turn("print 1", approver)
}

/// Runs a turn in which the model writes `src/main.rs` and calls `git_commit` with `message`.
fn write_and_commit_turn(
    client: &FakeChatClient,
    main_line: &mut MainLine,
    message: &str,
    approver: &mut dyn Approver,
) -> Turn {
    client.push_chat(Ok(write("write", "src/main.rs", PRINTS)));
    client.push_chat(Ok(git_commit("commit", message)));
    client.push_chat(Ok(reply("committed")));
    main_line.turn("print 1 and commit it", approver)
}

/// The tool result for the call `id` in the latest request.
fn result_of(client: &FakeChatClient, id: &str) -> String {
    client
        .requests()
        .last()
        .unwrap()
        .messages
        .iter()
        .find_map(|message| match message {
            Message::Tool {
                tool_call_id,
                content,
            } if tool_call_id == id => Some(content.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no tool result for {id}"))
}

// Not approved (#22 condition 1)

#[test]
fn a_git_commit_the_user_does_not_approve_adds_no_commit_and_stages_nothing() {
    let (dir, git) = repository();
    let client = FakeChatClient::new();
    let cargo = cargo(&[true, true]);
    let mut main_line = main_line(&client, &cargo, &dir, git);
    let mut answer = Answer::no();

    let turn = write_and_commit_turn(&client, &mut main_line, "Print 1", &mut answer);

    assert_eq!(turn.commits, [CommitOutcome::NotApproved]);
    assert_eq!(answer.proposals().len(), 1);
    assert_eq!(log(dir.path()), ["first"]);
    assert_eq!(
        status(dir.path()),
        " M src/main.rs\n",
        "the diff stays unstaged"
    );
    assert_eq!(
        git_subcommands(&main_line),
        ["status", "diff"],
        "only reads before the approval"
    );
    assert!(
        result_of(&client, "commit").starts_with("not committed: the user did not approve"),
        "{}",
        result_of(&client, "commit")
    );
}

#[test]
fn a_proposal_the_user_does_not_approve_adds_no_commit() {
    let (dir, git) = repository();
    let client = FakeChatClient::new();
    let cargo = cargo(&[true]);
    let mut main_line = main_line(&client, &cargo, &dir, git);
    let turn = write_turn(
        &client,
        &mut main_line,
        "src/main.rs",
        PRINTS,
        &mut Answer::no(),
    );
    client.push_chat(Ok(reply("Print 1")));
    let mut answer = Answer::no();

    let outcome = main_line.offer_commit(&turn, &mut answer);

    assert_eq!(outcome, Some(CommitOutcome::NotApproved));
    assert_eq!(answer.proposals().len(), 1);
    assert_eq!(log(dir.path()), ["first"]);
    assert_eq!(status(dir.path()), " M src/main.rs\n");
}

// Approved: only the files molt changed, and no push (#22 condition 2)

#[test]
fn an_approved_git_commit_commits_only_the_files_molt_changed_and_runs_no_push() {
    let (dir, git) = repository();
    let client = FakeChatClient::new();
    let cargo = cargo(&[true, true]);
    let mut main_line = main_line(&client, &cargo, &dir, git);
    let mut answer = Answer::yes();

    let turn = write_and_commit_turn(&client, &mut main_line, "Print 1", &mut answer);

    assert!(
        matches!(&turn.commits[..], [CommitOutcome::Committed { files, .. }] if files == &["src/main.rs"]),
        "{:?}",
        turn.commits
    );
    assert_eq!(log(dir.path()), ["Print 1", "first"]);
    assert_eq!(last_commit_files(dir.path()), ["src/main.rs"]);
    assert_eq!(status(dir.path()), "");
    let subcommands = git_subcommands(&main_line);
    assert_eq!(subcommands, ["status", "diff", "add", "commit"]);
    assert!(!subcommands.contains(&"push".to_owned()));
    assert!(result_of(&client, "commit").starts_with("committed: src/main.rs"));
}

#[test]
fn the_proposal_shows_the_files_against_head_with_the_models_message() {
    let (dir, git) = repository();
    let client = FakeChatClient::new();
    let cargo = cargo(&[true, true]);
    let mut main_line = main_line(&client, &cargo, &dir, git);
    let mut answer = Answer::no();

    write_and_commit_turn(&client, &mut main_line, "Print 1", &mut answer);

    let [proposal] = &answer.proposals()[..] else {
        panic!("{:?}", answer.asked);
    };
    assert_eq!(proposal.message, "Print 1");
    assert_eq!(proposal.files, ["src/main.rs"]);
    assert!(
        proposal
            .diff
            .contains("-fn main() {}\n+fn main() {\n+    println!(\"{}\", 1);\n+}\n"),
        "{}",
        proposal.diff
    );
    assert_eq!(proposal.new_files, Vec::<(String, String)>::new());
    assert_eq!(proposal.others, Vec::<String>::new());
    assert!(!proposal.check_failing);
}

#[test]
fn a_change_the_user_made_before_molt_changed_the_file_is_in_the_diff_the_commit_records() {
    let (dir, git) = repository();
    fs::write(dir.path().join("src/main.rs"), "// mine\nfn main() {}\n").unwrap();
    let client = FakeChatClient::new();
    let cargo = cargo(&[true]);
    let mut main_line = main_line(&client, &cargo, &dir, git);
    client.push_chat(Ok(call(
        "edit",
        "edit_file",
        json!({ "path": "src/main.rs", "old_string": "fn main() {}", "new_string": "fn main() {\n    println!(\"{}\", 1);\n}" }),
    )));
    client.push_chat(Ok(reply("done")));
    let turn = main_line.turn("print 1", &mut Answer::no());
    client.push_chat(Ok(reply("Print 1")));
    let mut answer = Answer::yes();

    main_line.offer_commit(&turn, &mut answer);

    let [proposal] = &answer.proposals()[..] else {
        panic!("{:?}", answer.asked);
    };
    assert!(proposal.diff.contains("+// mine\n"), "{}", proposal.diff);
    let committed = git_output(dir.path(), &["show", "--format=", "HEAD"]);
    assert!(committed.contains("+// mine\n"), "{committed}");
}

#[test]
fn a_file_git_does_not_track_is_shown_whole() {
    let (dir, git) = repository();
    let client = FakeChatClient::new();
    let cargo = cargo(&[true]);
    let mut main_line = main_line(&client, &cargo, &dir, git);
    let turn = write_turn(
        &client,
        &mut main_line,
        "src/lib.rs",
        "pub fn one() {}\n",
        &mut Answer::no(),
    );
    client.push_chat(Ok(reply("Add one")));
    let mut answer = Answer::no();

    main_line.offer_commit(&turn, &mut answer);

    let [proposal] = &answer.proposals()[..] else {
        panic!("{:?}", answer.asked);
    };
    assert_eq!(proposal.diff, "");
    assert_eq!(
        proposal.new_files,
        [("src/lib.rs".to_owned(), "pub fn one() {}\n".to_owned())]
    );
}

#[test]
fn in_a_repository_without_a_commit_every_file_is_shown_whole() {
    let dir = project();
    let git = init(dir.path());
    run_git(dir.path(), &["add", "Cargo.toml", "Cargo.lock"]);
    let client = FakeChatClient::new();
    let cargo = cargo(&[true]);
    let mut main_line = main_line(&client, &cargo, &dir, git);
    let turn = write_turn(
        &client,
        &mut main_line,
        "Cargo.toml",
        &format!("{MANIFEST}# edited\n"),
        &mut Answer::no(),
    );
    client.push_chat(Ok(reply("Edit the manifest")));
    let mut answer = Answer::yes();

    let outcome = main_line.offer_commit(&turn, &mut answer);

    let [proposal] = &answer.proposals()[..] else {
        panic!("{:?}", answer.asked);
    };
    assert_eq!(proposal.diff, "");
    assert_eq!(
        proposal.new_files,
        [("Cargo.toml".to_owned(), format!("{MANIFEST}# edited\n"))]
    );
    assert!(
        matches!(outcome, Some(CommitOutcome::Committed { .. })),
        "{outcome:?}"
    );
    assert_eq!(log(dir.path()), ["Edit the manifest"]);
}

// Other changes (#22 condition 3)

#[test]
fn other_changes_are_reported_and_stay_as_they_are_after_the_commit() {
    let (dir, git) = repository();
    fs::write(dir.path().join("README.md"), "mine\n").unwrap();
    run_git(dir.path(), &["add", "README.md"]);
    fs::write(dir.path().join("notes.txt"), "mine\n").unwrap();
    fs::create_dir(dir.path().join("drafts")).unwrap();
    fs::write(dir.path().join("drafts/a.rs"), "// mine\n").unwrap();
    let client = FakeChatClient::new();
    let cargo = cargo(&[true]);
    let mut main_line = main_line(&client, &cargo, &dir, git);
    let turn = write_turn(
        &client,
        &mut main_line,
        "src/main.rs",
        PRINTS,
        &mut Answer::no(),
    );
    client.push_chat(Ok(reply("Print 1")));
    let mut answer = Answer::yes();

    let outcome = main_line.offer_commit(&turn, &mut answer);

    assert!(
        matches!(outcome, Some(CommitOutcome::Committed { .. })),
        "{outcome:?}"
    );
    let [proposal] = &answer.proposals()[..] else {
        panic!("{:?}", answer.asked);
    };
    assert_eq!(proposal.others, ["README.md", "drafts/a.rs", "notes.txt"]);
    assert_eq!(last_commit_files(dir.path()), ["src/main.rs"]);
    assert_eq!(
        status(dir.path()),
        "A  README.md\n?? drafts/a.rs\n?? notes.txt\n"
    );
}

// cargo add (#22 condition 4)

/// A cargo whose `cargo add` changes `Cargo.toml` and `Cargo.lock` as the real one does, and whose
/// `cargo check` passes.
struct AddingCargo {
    dir: PathBuf,
    calls: RefCell<Vec<CargoCall>>,
}

impl CargoRunner for AddingCargo {
    fn run(&self, subcommand: Subcommand) -> Result<CargoOutput, CargoError> {
        self.calls.borrow_mut().push(CargoCall::Run(subcommand));
        output(true)
    }

    fn add(&self, crate_name: &CrateName, _approval: Approval) -> Result<CargoOutput, CargoError> {
        self.calls
            .borrow_mut()
            .push(CargoCall::Add(crate_name.clone()));
        let name = crate_name.as_str();
        let manifest = self.dir.join("Cargo.toml");
        let text = fs::read_to_string(&manifest).unwrap();
        fs::write(manifest, format!("{text}{name} = \"1\"\n")).unwrap();
        let lock = self.dir.join("Cargo.lock");
        let text = fs::read_to_string(&lock).unwrap();
        fs::write(lock, format!("{text}\n[[package]]\nname = \"{name}\"\n")).unwrap();
        output(true)
    }
}

#[test]
fn the_commit_after_an_approved_cargo_add_has_cargo_toml_and_cargo_lock() {
    let (dir, git) = repository();
    let client = FakeChatClient::new();
    let cargo = AddingCargo {
        dir: dir.path().to_owned(),
        calls: RefCell::new(Vec::new()),
    };
    let mut main_line = main_line(&client, &cargo, &dir, git);
    client.push_chat(Ok(call(
        "add",
        "run_cargo",
        json!({ "subcommand": "add", "args": ["serde"] }),
    )));
    client.push_chat(Ok(reply("added serde")));
    let turn = main_line.turn("add serde", &mut Answer::yes());
    client.push_chat(Ok(reply("Add serde")));

    let outcome = main_line.offer_commit(&turn, &mut Answer::yes());

    assert!(
        matches!(outcome, Some(CommitOutcome::Committed { .. })),
        "{outcome:?}"
    );
    assert_eq!(last_commit_files(dir.path()), ["Cargo.lock", "Cargo.toml"]);
    assert_eq!(status(dir.path()), "");
}

/// A cargo whose `cargo check` passes and changes `Cargo.lock` each time, as cargo may on its own.
struct LockingCargo {
    dir: PathBuf,
    checks: RefCell<usize>,
}

impl CargoRunner for LockingCargo {
    fn run(&self, _subcommand: Subcommand) -> Result<CargoOutput, CargoError> {
        *self.checks.borrow_mut() += 1;
        let lock = format!("{LOCK}# check {}\n", self.checks.borrow());
        fs::write(self.dir.join("Cargo.lock"), lock).unwrap();
        output(true)
    }

    fn add(&self, _crate_name: &CrateName, _approval: Approval) -> Result<CargoOutput, CargoError> {
        output(true)
    }
}

#[test]
fn cargo_lock_joins_the_changes_of_the_turn_only_along_with_cargo_toml() {
    let (dir, git) = repository();
    let client = FakeChatClient::new();
    let cargo = LockingCargo {
        dir: dir.path().to_owned(),
        checks: RefCell::new(0),
    };
    let mut main_line = main_line(&client, &cargo, &dir, git);

    let turn = write_turn(
        &client,
        &mut main_line,
        "src/main.rs",
        PRINTS,
        &mut Answer::no(),
    );
    let with_manifest = write_turn(
        &client,
        &mut main_line,
        "Cargo.toml",
        &format!("{MANIFEST}# edited\n"),
        &mut Answer::no(),
    );

    let paths =
        |turn: &Turn| -> Vec<String> { turn.changes.iter().map(|c| c.path.clone()).collect() };
    assert_eq!(paths(&turn), ["src/main.rs"]);
    assert_eq!(paths(&with_manifest), ["Cargo.toml", "Cargo.lock"]);
}

// Nothing to commit, and no repository (#22 condition 5)

#[test]
fn git_commit_with_nothing_molt_changed_reports_it_and_runs_only_git_status() {
    let (dir, git) = repository();
    let client = FakeChatClient::new();
    let cargo = FakeCargo::new();
    let mut main_line = main_line(&client, &cargo, &dir, git);
    client.push_chat(Ok(git_commit("commit", "Nothing")));
    client.push_chat(Ok(reply("nothing")));
    let mut answer = Answer::yes();

    let turn = main_line.turn("commit", &mut answer);

    assert_eq!(turn.commits, [CommitOutcome::NothingToCommit]);
    assert!(answer.asked.is_empty());
    assert_eq!(git_subcommands(&main_line), ["status"]);
    assert_eq!(log(dir.path()), ["first"]);
    assert!(result_of(&client, "commit").starts_with("not committed: none of the files"));
}

#[test]
fn a_file_molt_wrote_back_as_git_has_it_is_nothing_to_commit() {
    let (dir, git) = repository();
    let client = FakeChatClient::new();
    let cargo = cargo(&[true, true]);
    let mut main_line = main_line(&client, &cargo, &dir, git);
    client.push_chat(Ok(write("write", "src/main.rs", PRINTS)));
    client.push_chat(Ok(write("undo", "src/main.rs", MAIN)));
    client.push_chat(Ok(git_commit("commit", "Nothing")));
    client.push_chat(Ok(reply("nothing")));
    let mut answer = Answer::yes();

    let turn = main_line.turn("print 1, then undo it", &mut answer);

    assert_eq!(turn.commits, [CommitOutcome::NothingToCommit]);
    assert!(answer.asked.is_empty());
    assert_eq!(main_line.offer_commit(&turn, &mut answer), None);
}

#[test]
fn git_commit_outside_a_git_repository_reports_it_and_commits_nothing() {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = cargo(&[true, true]);
    let mut main_line = main_line(&client, &cargo, &dir, without_repository(dir.path()));
    let mut answer = Answer::yes();

    let turn = write_and_commit_turn(&client, &mut main_line, "Print 1", &mut answer);

    assert!(
        matches!(&turn.commits[..], [CommitOutcome::NotARepository(error)] if error.contains("not a git repository")),
        "{:?}",
        turn.commits
    );
    assert!(answer.asked.is_empty());
    assert_eq!(git_subcommands(&main_line), ["status"]);
    assert!(!dir.path().join(".git").exists());
    assert!(
        result_of(&client, "commit")
            .starts_with("not committed: the project is not in a git repository")
    );
}

#[test]
fn outside_a_git_repository_molt_proposes_nothing_and_asks_no_model() {
    let dir = project();
    let client = FakeChatClient::new();
    let cargo = cargo(&[true]);
    let mut main_line = main_line(&client, &cargo, &dir, without_repository(dir.path()));
    let turn = write_turn(
        &client,
        &mut main_line,
        "src/main.rs",
        PRINTS,
        &mut Answer::no(),
    );
    let requests = client.requests().len();
    let mut answer = Answer::yes();

    assert_eq!(main_line.offer_commit(&turn, &mut answer), None);

    assert!(answer.asked.is_empty());
    assert_eq!(client.requests().len(), requests);
}

// The last cargo check failing (#22 condition 6)

#[test]
fn git_commit_after_a_write_runs_cargo_check_first_and_warns_when_it_fails() {
    let (dir, git) = repository();
    let client = FakeChatClient::new();
    // The check before the commit, then the one at the end of the turn.
    let cargo = cargo(&[false, false]);
    let mut main_line = main_line(&client, &cargo, &dir, git);
    client.push_chat(Ok(write("write", "src/main.rs", PRINTS)));
    client.push_chat(Ok(git_commit("commit", "Print 1")));
    client.push_chat(Ok(reply("committed")));
    client.push_chat(Ok(reply("cannot fix it")));
    let mut answer = Answer::no();

    main_line.turn("print 1 and commit it", &mut answer);

    let [proposal] = &answer.proposals()[..] else {
        panic!("{:?}", answer.asked);
    };
    assert!(proposal.check_failing);
    assert_eq!(cargo.calls(), vec![CargoCall::Run(Subcommand::Check); 2]);
}

#[test]
fn after_a_turn_whose_check_still_fails_git_commit_warns_and_molt_proposes_nothing() {
    let (dir, git) = repository();
    let client = FakeChatClient::new();
    let cargo = cargo(&[false]);
    let mut main_line = main_line(&client, &cargo, &dir, git);
    client.push_chat(Ok(write("write", "src/main.rs", PRINTS)));
    client.push_chat(Ok(reply("done")));
    client.push_chat(Ok(reply("cannot fix it")));
    let turn = main_line.turn("print 1", &mut Answer::no());
    assert_eq!(main_line.offer_commit(&turn, &mut Answer::yes()), None);
    client.push_chat(Ok(git_commit("commit", "Print 1")));
    client.push_chat(Ok(reply("committed")));
    let mut answer = Answer::yes();

    let turn = main_line.turn("commit it anyway", &mut answer);

    let [proposal] = &answer.proposals()[..] else {
        panic!("{:?}", answer.asked);
    };
    assert!(proposal.check_failing);
    assert!(matches!(
        &turn.commits[..],
        [CommitOutcome::Committed { .. }]
    ));
    assert_eq!(log(dir.path()), ["Print 1", "first"]);
}

// git commit fails (#22 condition 7)

#[test]
fn a_commit_git_refuses_shows_the_error_commits_nothing_and_leaves_the_files_staged() {
    let (dir, git) = repository_with(init_without_identity);
    let client = FakeChatClient::new();
    let cargo = cargo(&[true, true]);
    let mut main_line = main_line(&client, &cargo, &dir, git);

    let turn = write_and_commit_turn(&client, &mut main_line, "Print 1", &mut Answer::yes());

    let [CommitOutcome::Failed { staged, error }] = &turn.commits[..] else {
        panic!("{:?}", turn.commits);
    };
    assert!(staged);
    assert!(error.contains("user.name"), "{error}");
    assert_eq!(log(dir.path()), ["first"]);
    assert_eq!(status(dir.path()), "M  src/main.rs\n");
    let result = result_of(&client, "commit");
    assert!(
        result.starts_with("failed: nothing was committed. The files stay staged."),
        "{result}"
    );
}

// When molt proposes, and its message

#[test]
fn after_a_finished_turn_that_wrote_and_passed_molt_proposes_the_generate_models_message() {
    let (dir, git) = repository();
    let client = FakeChatClient::new();
    let cargo = cargo(&[true]);
    let mut main_line = main_line(&client, &cargo, &dir, git);
    let turn = write_turn(
        &client,
        &mut main_line,
        "src/main.rs",
        PRINTS,
        &mut Answer::no(),
    );
    client.push_chat(Ok(reply("```\nPrint 1 from main\n```")));
    let mut answer = Answer::yes();

    let outcome = main_line.offer_commit(&turn, &mut answer);

    assert!(
        matches!(outcome, Some(CommitOutcome::Committed { .. })),
        "{outcome:?}"
    );
    assert_eq!(log(dir.path()), ["Print 1 from main", "first"]);
    let request = client.requests().last().unwrap().clone();
    assert_eq!(request.model, Config::default().models.generate);
    assert!(request.tools.is_empty());
    let Message::User { content } = &request.messages[1] else {
        panic!("{:?}", request.messages);
    };
    assert!(content.contains("print 1"), "the request: {content}");
    assert!(
        content.contains("+++ b/src/main.rs") && content.contains("-fn main() {}\n+fn main() {"),
        "{content}"
    );
}

#[test]
fn ctrl_c_while_waiting_for_the_message_drops_the_proposal() {
    let (dir, git) = repository();
    let client = FakeChatClient::new();
    let cargo = cargo(&[true]);
    let mut main_line = main_line(&client, &cargo, &dir, git);
    let turn = write_turn(
        &client,
        &mut main_line,
        "src/main.rs",
        PRINTS,
        &mut Answer::no(),
    );
    client.push_chat(Err(ChatError::Interrupted));
    let mut answer = Answer::yes();

    assert_eq!(main_line.offer_commit(&turn, &mut answer), None);

    assert!(answer.asked.is_empty());
    assert_eq!(log(dir.path()), ["first"]);
    assert_eq!(status(dir.path()), " M src/main.rs\n");
}

#[test]
fn when_the_model_gives_no_message_the_proposal_has_the_fixed_one() {
    for response in [Ok(reply("  ")), Err(ChatError::FakeExhausted)] {
        let (dir, git) = repository();
        let client = FakeChatClient::new();
        let cargo = cargo(&[true]);
        let mut main_line = main_line(&client, &cargo, &dir, git);
        let turn = write_turn(
            &client,
            &mut main_line,
            "src/main.rs",
            PRINTS,
            &mut Answer::no(),
        );
        client.push_chat(response);
        let mut answer = Answer::no();

        main_line.offer_commit(&turn, &mut answer);

        assert_eq!(answer.proposals()[0].message, "molt: src/main.rs");
    }
}

#[test]
fn a_turn_that_changed_nothing_gets_no_proposal_and_runs_no_git() {
    let (dir, git) = repository();
    let client = FakeChatClient::new();
    let cargo = FakeCargo::new();
    let mut main_line = main_line(&client, &cargo, &dir, git);
    client.push_chat(Ok(reply("a slice is a view")));
    let turn = main_line.turn("what is a slice?", &mut Answer::no());

    assert_eq!(main_line.offer_commit(&turn, &mut Answer::yes()), None);

    assert!(git_subcommands(&main_line).is_empty());
}

#[test]
fn a_turn_in_which_the_model_called_git_commit_gets_no_proposal() {
    let (dir, git) = repository();
    let client = FakeChatClient::new();
    let cargo = cargo(&[true, true]);
    let mut main_line = main_line(&client, &cargo, &dir, git);
    let turn = write_and_commit_turn(&client, &mut main_line, "Print 1", &mut Answer::no());
    let mut answer = Answer::yes();

    assert_eq!(main_line.offer_commit(&turn, &mut answer), None);

    assert!(answer.asked.is_empty());
    assert_eq!(log(dir.path()), ["first"]);
}

#[test]
fn a_turn_that_stopped_gets_no_proposal() {
    let (dir, git) = repository();
    let client = FakeChatClient::new();
    let cargo = cargo(&[true]);
    let mut main_line = main_line(&client, &cargo, &dir, git);
    client.push_chat(Ok(write("write", "src/main.rs", PRINTS)));
    client.push_chat(Err(ChatError::FakeExhausted));
    let turn = main_line.turn("print 1", &mut Answer::no());
    assert!(matches!(turn.end, TurnEnd::ApiFailed(_)), "{:?}", turn.end);

    assert_eq!(main_line.offer_commit(&turn, &mut Answer::yes()), None);
}

// What the files molt changed are, across turns

#[test]
fn a_declined_proposal_comes_again_with_the_files_of_the_next_turn() {
    let (dir, git) = repository();
    let client = FakeChatClient::new();
    let cargo = cargo(&[true, true]);
    let mut main_line = main_line(&client, &cargo, &dir, git);
    let turn = write_turn(
        &client,
        &mut main_line,
        "src/main.rs",
        PRINTS,
        &mut Answer::no(),
    );
    client.push_chat(Ok(reply("Print 1")));
    main_line.offer_commit(&turn, &mut Answer::no());
    let turn = write_turn(
        &client,
        &mut main_line,
        "src/lib.rs",
        "pub fn one() {}\n",
        &mut Answer::no(),
    );
    client.push_chat(Ok(reply("Print 1 and add one")));

    let outcome = main_line.offer_commit(&turn, &mut Answer::yes());

    assert!(
        matches!(outcome, Some(CommitOutcome::Committed { .. })),
        "{outcome:?}"
    );
    assert_eq!(last_commit_files(dir.path()), ["src/lib.rs", "src/main.rs"]);
}

#[test]
fn a_file_git_commit_committed_is_not_proposed_again_at_the_end_of_the_turn() {
    let (dir, git) = repository();
    let client = FakeChatClient::new();
    let cargo = cargo(&[true, true]);
    let mut main_line = main_line(&client, &cargo, &dir, git);
    let turn = write_and_commit_turn(&client, &mut main_line, "Print 1", &mut Answer::yes());
    let requests = client.requests().len();

    assert_eq!(main_line.offer_commit(&turn, &mut Answer::yes()), None);

    assert_eq!(
        client.requests().len(),
        requests,
        "no message was asked for"
    );
    assert_eq!(log(dir.path()), ["Print 1", "first"]);
}

#[test]
fn a_file_changed_again_after_its_commit_is_proposed_from_its_committed_content() {
    let (dir, git) = repository();
    let client = FakeChatClient::new();
    let cargo = cargo(&[true, true, true]);
    let mut main_line = main_line(&client, &cargo, &dir, git);
    client.push_chat(Ok(write("write", "src/main.rs", PRINTS)));
    client.push_chat(Ok(git_commit("commit", "Print 1")));
    client.push_chat(Ok(write("again", "src/main.rs", PRINTS_2)));
    client.push_chat(Ok(reply("done")));
    main_line.turn("print 1, commit, then print 2", &mut Answer::yes());
    client.push_chat(Ok(git_commit("commit", "Print 2")));
    client.push_chat(Ok(reply("not committed")));
    let mut answer = Answer::no();

    main_line.turn("commit it", &mut answer);

    let [proposal] = &answer.proposals()[..] else {
        panic!("{:?}", answer.asked);
    };
    assert_eq!(proposal.files, ["src/main.rs"]);
    assert!(
        proposal
            .diff
            .contains("-    println!(\"{}\", 1);\n+    println!(\"{}\", 2);\n"),
        "{}",
        proposal.diff
    );
}

#[test]
fn what_molt_wrote_before_a_withdrawn_boundary_is_proposed_with_the_next_commit() {
    let (dir, git) = repository();
    let client = FakeChatClient::new();
    let cargo = cargo(&[true]);
    let mut main_line = main_line(&client, &cargo, &dir, git);
    client.push_chat(Ok(write("write", "src/lib.rs", "pub fn one() {}\n")));
    client.push_chat(Ok(call(
        "boundary",
        "request_boundary",
        json!({ "items": ["dbg_macro"], "reason": "needed" }),
    )));
    let turn = main_line.turn("add one and debug it", &mut Answer::no());
    assert!(matches!(turn.end, TurnEnd::Boundary(_)), "{:?}", turn.end);
    main_line.withdraw();
    client.push_chat(Ok(git_commit("commit", "Add one")));
    client.push_chat(Ok(reply("committed")));

    main_line.turn("commit what you wrote", &mut Answer::yes());

    assert_eq!(last_commit_files(dir.path()), ["src/lib.rs"]);
}

// The two cases #58 left open: ③ (UC3) and the files molt changed

/// Stops a turn at a boundary on `dbg_macro` that the model asks for, without writing.
fn stop_at_a_boundary(
    client: &FakeChatClient,
    main_line: &mut MainLine,
) -> crate::agent::mainline::Boundary {
    client.push_chat(Ok(call(
        "boundary",
        "request_boundary",
        json!({ "items": ["dbg_macro"], "reason": "needed" }),
    )));
    match main_line.turn("debug 1", &mut Answer::no()).end {
        TurnEnd::Boundary(boundary) => boundary,
        end => panic!("{end:?}"),
    }
}

#[test]
fn a_turn_whose_only_change_is_what_the_user_wrote_in_3_gets_a_proposal_of_it() {
    let (dir, git) = repository();
    let client = FakeChatClient::new();
    // The check of `/done`.
    let cargo = cargo(&[true]);
    let mut main_line = main_line(&client, &cargo, &dir, git);
    let boundary = stop_at_a_boundary(&client, &mut main_line);
    let mut side = main_line.side_line(boundary);
    fs::write(dir.path().join("src/main.rs"), WITH_DBG).unwrap();
    client.push_chat(Ok(reply("review")));
    let Done::Passed { resolution, .. } = main_line.done(&mut side) else {
        panic!("/done did not pass");
    };
    client.push_chat(Ok(reply("done")));
    let resumed = main_line
        .resolve(&resolution, &mut Answer::no())
        .unwrap()
        .resumed();
    assert!(resumed.turn.written.is_empty(), "molt wrote nothing");
    client.push_chat(Ok(reply("Debug 1")));
    let mut answer = Answer::yes();

    let outcome = main_line.offer_commit(&resumed.turn, &mut answer);

    assert!(
        matches!(outcome, Some(CommitOutcome::Committed { .. })),
        "{outcome:?}"
    );
    assert!(!answer.proposals()[0].check_failing);
    assert_eq!(last_commit_files(dir.path()), ["src/main.rs"]);
}

#[test]
fn a_file_the_user_wrote_in_3_without_passing_done_is_not_among_the_files_molt_changed() {
    let (dir, git) = repository();
    let client = FakeChatClient::new();
    let cargo = cargo(&[true]);
    let mut main_line = main_line(&client, &cargo, &dir, git);
    let boundary = stop_at_a_boundary(&client, &mut main_line);
    let side = main_line.side_line(boundary);
    // The user starts ③ in src/main.rs, then switches to ①.
    fs::write(dir.path().join("src/main.rs"), WITH_DBG).unwrap();
    let resolution = side
        .resolve(Choice::Unlock, &SessionLog::disabled())
        .unwrap();
    client.push_chat(Ok(write("write", "src/lib.rs", "pub fn one() {}\n")));
    client.push_chat(Ok(reply("done")));
    let resumed = main_line
        .resolve(&resolution, &mut Answer::no())
        .unwrap()
        .resumed();
    client.push_chat(Ok(reply("Add one")));
    let mut answer = Answer::yes();

    main_line.offer_commit(&resumed.turn, &mut answer);

    let [proposal] = &answer.proposals()[..] else {
        panic!("{:?}", answer.asked);
    };
    let paths = proposal.files.clone();
    assert_eq!(paths, ["src/lib.rs"]);
    assert_eq!(proposal.others, ["src/main.rs"]);
    assert_eq!(last_commit_files(dir.path()), ["src/lib.rs"]);
    assert_eq!(status(dir.path()), " M src/main.rs\n");
}

#[test]
fn lines_the_user_wrote_in_3_without_passing_done_come_along_when_molt_changes_that_file_too() {
    let (dir, git) = repository();
    let client = FakeChatClient::new();
    let cargo = cargo(&[true]);
    let mut main_line = main_line(&client, &cargo, &dir, git);
    let boundary = stop_at_a_boundary(&client, &mut main_line);
    let side = main_line.side_line(boundary);
    fs::write(dir.path().join("src/main.rs"), WITH_DBG).unwrap();
    let resolution = side
        .resolve(Choice::Unlock, &SessionLog::disabled())
        .unwrap();
    client.push_chat(Ok(call(
        "edit",
        "edit_file",
        json!({ "path": "src/main.rs", "old_string": "fn main() {", "new_string": "fn main() {\n    println!(\"{}\", 1);" }),
    )));
    client.push_chat(Ok(reply("done")));
    let resumed = main_line
        .resolve(&resolution, &mut Answer::no())
        .unwrap()
        .resumed();
    client.push_chat(Ok(reply("Print 1")));
    let mut answer = Answer::no();

    main_line.offer_commit(&resumed.turn, &mut answer);

    let [proposal] = &answer.proposals()[..] else {
        panic!("{:?}", answer.asked);
    };
    assert_eq!(proposal.files, ["src/main.rs"]);
    assert!(
        proposal.diff.contains("+    dbg!(1);\n"),
        "{}",
        proposal.diff
    );
}

#[test]
fn a_file_the_user_wrote_in_3_before_switching_to_2_is_not_among_the_files_molt_changed() {
    let (dir, git) = repository();
    let client = FakeChatClient::new();
    let cargo = cargo(&[true]);
    let mut main_line = main_line(&client, &cargo, &dir, git);
    let boundary = stop_at_a_boundary(&client, &mut main_line);
    let side = main_line.side_line(boundary);
    fs::write(dir.path().join("src/main.rs"), WITH_DBG).unwrap();
    // The policy of ② from the light model.
    client.push_chat(Ok(reply("Print the value with println! instead.")));
    let resolution = side
        .resolve(Choice::Rewrite, &SessionLog::disabled())
        .unwrap();
    client.push_chat(Ok(write("write", "src/lib.rs", "pub fn one() {}\n")));
    client.push_chat(Ok(reply("done")));
    let resumed = main_line
        .resolve(&resolution, &mut Answer::no())
        .unwrap()
        .resumed();
    client.push_chat(Ok(reply("Add one")));
    let mut answer = Answer::no();

    main_line.offer_commit(&resumed.turn, &mut answer);

    let [proposal] = &answer.proposals()[..] else {
        panic!("{:?}", answer.asked);
    };
    let paths = proposal.files.clone();
    assert_eq!(paths, ["src/lib.rs"]);
    assert_eq!(proposal.others, ["src/main.rs"]);
}

#[test]
fn a_file_the_user_wrote_in_3_before_withdrawing_the_request_is_not_among_the_files_molt_changed() {
    let (dir, git) = repository();
    let client = FakeChatClient::new();
    let cargo = FakeCargo::new();
    let mut main_line = main_line(&client, &cargo, &dir, git);
    let boundary = stop_at_a_boundary(&client, &mut main_line);
    let _side = main_line.side_line(boundary);
    fs::write(dir.path().join("src/main.rs"), WITH_DBG).unwrap();
    main_line.withdraw();
    client.push_chat(Ok(git_commit("commit", "Debug 1")));
    client.push_chat(Ok(reply("nothing")));
    let mut answer = Answer::yes();

    let turn = main_line.turn("commit it", &mut answer);

    assert_eq!(turn.commits, [CommitOutcome::NothingToCommit]);
    assert!(answer.asked.is_empty());
    assert_eq!(status(dir.path()), " M src/main.rs\n");
}

// Reading git's and the model's output

#[test]
fn status_entries_reads_renames_quoted_paths_and_their_status() {
    let status = " M src/main.rs\nR  old.rs -> new.rs\n?? \"caf\\303\\251.rs\"\n?? \"a\\\"b.rs\"\n";
    assert_eq!(
        status_entries(status),
        [
            ("src/main.rs".to_owned(), " M".to_owned()),
            ("new.rs".to_owned(), "R ".to_owned()),
            ("café.rs".to_owned(), "??".to_owned()),
            ("a\"b.rs".to_owned(), "??".to_owned())
        ]
    );
}

#[test]
fn unfenced_takes_the_message_out_of_a_code_fence() {
    assert_eq!(unfenced("  Add one\n"), "Add one");
    assert_eq!(unfenced("```\nAdd one\n\nBody\n```"), "Add one\n\nBody");
    assert_eq!(unfenced("```text\nAdd one\n```\n"), "Add one");
}
