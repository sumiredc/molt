use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use serde_json::json;
use tempfile::TempDir;

use super::super::super::tests::{input, project, reply};
use super::super::super::{PAUSED, main_line};
use super::*;
use crate::agent::tools::Tools;
use crate::config::Config;
use crate::llm::{ChatResponse, FakeChatClient, Message, Router, ToolCall};
use crate::profile::status::{Provenance, Status};
use crate::profile::store::Profile;
use crate::ui::{Input, Picked, ScriptedUi, Step, UiError};
use crate::workspace::cargo::{CargoOutput, FakeCargo};
use crate::workspace::path::Root;

const WITH_DBG: &str = "fn main() {\n    dbg!(1);\n}\n";
const SUB: &str = "molt [sub: dbg_macro]> ";

fn write_main(id: &str) -> ChatResponse {
    ChatResponse {
        content: None,
        tool_calls: vec![ToolCall {
            id: id.to_owned(),
            name: "write_file".to_owned(),
            arguments: json!({ "path": "src/main.rs", "content": WITH_DBG }).to_string(),
        }],
    }
}

/// A model that tries `dbg!` twice, which stops the first turn at a boundary, explains it on the
/// side line, then answers with each of `then` in turn.
fn stops_at_a_boundary_then(then: Vec<Result<ChatResponse, ChatError>>) -> FakeChatClient {
    let client = FakeChatClient::new();
    client.push_chat(Ok(write_main("first")));
    client.push_chat(Ok(reply("explanation")));
    for response in then {
        client.push_chat(response);
    }
    client
}

/// A [`ScriptedUi`] whose user writes files in their editor right before they send `/done`: the
/// files of the first entry of `writes` before the first `/done`, and so on.
struct Writing {
    ui: ScriptedUi,
    writes: VecDeque<Vec<(PathBuf, &'static str)>>,
}

impl Ui for Writing {
    fn read_line(&mut self, prompt: &str) -> Result<Input, UiError> {
        self.ui.read_line(prompt)
    }

    fn show(&mut self, text: &str) {
        self.ui.show(text);
    }

    fn choose(
        &mut self,
        prompt: &str,
        choices: &[(usize, &str)],
        line_prompt: &str,
    ) -> Result<Picked, UiError> {
        let picked = self.ui.choose(prompt, choices, line_prompt)?;
        if matches!(&picked, Picked::Line(line) if line == "/done") {
            for (path, content) in self.writes.pop_front().unwrap_or_default() {
                fs::write(path, content).unwrap();
            }
        }
        Ok(picked)
    }

    fn confirm(&mut self, prompt: &str) -> Result<Option<bool>, UiError> {
        self.ui.confirm(prompt)
    }
    fn form(
        &mut self,
        form: &crate::ui::Form,
        initial: Option<&crate::ui::FormValues>,
    ) -> Result<Option<crate::ui::FormValues>, UiError> {
        self.ui.form(form, initial)
    }
}

/// A project with `src/main.rs` and an empty `profile.toml`.
fn project_with_a_profile() -> TempDir {
    let dir = project();
    fs::create_dir(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
    fs::write(dir.path().join("profile.toml"), "").unwrap();
    dir
}

/// An error of `cargo check` on line 2 of `src/main.rs`, where the user writes in these tests.
const IN_MAIN: &str = "error[E0425]: x\n --> src/main.rs:2:10";

fn check(passed: bool) -> CargoOutput {
    CargoOutput {
        success: passed,
        stdout: String::new(),
        stderr: if passed { "" } else { IN_MAIN }.to_owned(),
    }
}

/// Runs the main line on `steps` in `dir`, with the profile read from and saved to its
/// `profile.toml`, a cargo whose `cargo check`s give `checks` in turn, and a user who writes
/// `writes` (relative to `dir`) before each `/done`.
fn run_in(
    dir: &TempDir,
    client: &FakeChatClient,
    checks: &[bool],
    writes: Vec<Vec<(&str, &'static str)>>,
    steps: impl IntoIterator<Item = Step>,
) -> (ExitCode, ScriptedUi) {
    let path = dir.path().join("profile.toml");
    let cargo = FakeCargo::new();
    for &passed in checks {
        cargo.push(Ok(check(passed)));
    }
    let profile = Profile::load(&path).unwrap();
    let tools = Tools::new(Root::new(dir.path()).unwrap(), &cargo);
    let mut agent = MainLine::new(client, Router::new(&Config::default()), tools, profile)
        .with_profile_path(path);
    let mut ui = Writing {
        ui: ScriptedUi::new(steps),
        writes: writes
            .into_iter()
            .map(|files| {
                files
                    .into_iter()
                    .map(|(file, content)| (dir.path().join(file), content))
                    .collect()
            })
            .collect(),
    };
    let code = main_line(&mut ui, &mut agent);
    (code, ui.ui)
}

/// The status and provenance of `dbg_macro` in the saved profile.
fn saved(dir: &TempDir) -> Option<(Status, Provenance)> {
    let profile = Profile::load(&dir.path().join("profile.toml")).unwrap();
    profile
        .entry(&"dbg_macro".parse().unwrap())
        .map(|entry| (entry.status(), entry.provenance()))
}

fn shown_at(ui: &ScriptedUi, text: &str) -> usize {
    let shown = ui.shown();
    shown
        .iter()
        .position(|shown| shown == text)
        .unwrap_or_else(|| panic!("{text:?} not in {shown:?}"))
}

/// The last user message the `generate` model was sent on the main line.
fn last_main_line_input(client: &FakeChatClient) -> String {
    let generate = Config::default().models.generate;
    let request = client
        .requests()
        .into_iter()
        .rfind(|request| request.model == generate && request.messages.len() > 2)
        .unwrap();
    request
        .messages
        .iter()
        .rev()
        .find_map(|message| match message {
            Message::User { content } => Some(content.clone()),
            _ => None,
        })
        .unwrap()
}

#[test]
fn choosing_3_guides_and_done_saves_the_items_as_written_and_the_main_line_goes_on() {
    let dir = project_with_a_profile();
    let client = stops_at_a_boundary_then(vec![
        Ok(reply("Write dbg!(1) in main of src/main.rs.")),
        Ok(reply("Good use of dbg!.")),
        Ok(reply("Read it; the request is done.")),
    ]);

    let (code, ui) = run_in(
        &dir,
        &client,
        &[true],
        vec![vec![("src/main.rs", WITH_DBG)]],
        [
            input("debug 1"),
            Step::Select(3),
            input("/done"),
            input("/quit"),
        ],
    );

    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(saved(&dir), Some((Status::Known, Provenance::WroteIt)));
    let guidance = shown_at(&ui, "  Write dbg!(1) in main of src/main.rs.");
    assert_eq!(ui.shown()[guidance + 1], format!("  {WRITING}"));
    let passed = shown_at(&ui, "  cargo check passed.");
    assert_eq!(ui.shown()[passed + 1], "  Good use of dbg!.");
    let known = shown_at(
        &ui,
        "Your profile has dbg_macro as known now (you wrote it).",
    );
    assert_eq!(ui.shown()[known + 1], "Read it; the request is done.");
    assert!(
        ui.shown()[known + 2].starts_with("--- a/src/main.rs\n+++ b/src/main.rs\n"),
        "the user's file is among the turn's changes: {:?}",
        ui.shown()
    );
    assert!(last_main_line_input(&client).starts_with("BOUNDARY RESOLVED: user_wrote\n"));
    assert_eq!(
        ui.asked(),
        ["molt> ", "How do you want to go on?", SUB, "molt> "]
    );
}

#[test]
fn a_failed_check_on_done_is_shown_and_the_user_can_send_done_again() {
    let dir = project_with_a_profile();
    let client = stops_at_a_boundary_then(vec![
        Ok(reply("guidance")),
        Ok(reply("review")),
        Ok(reply("done")),
    ]);

    let (code, ui) = run_in(
        &dir,
        &client,
        &[false, true],
        vec![
            vec![("src/main.rs", "fn main() {\n    dbg!(x);\n}\n")],
            vec![("src/main.rs", WITH_DBG)],
        ],
        [
            input("debug 1"),
            Step::Select(3),
            input("/done"),
            input("/done"),
            input("/quit"),
        ],
    );

    assert_eq!(code, ExitCode::SUCCESS);
    let failed = shown_at(
        &ui,
        "  cargo check found errors in your lines:\n  error[E0425]: x\n   --> src/main.rs:2:10",
    );
    assert_eq!(
        ui.shown()[failed + 1],
        "  Your profile is unchanged. Fix it and type /done again, or choose another way."
    );
    assert_eq!(saved(&dir), Some((Status::Known, Provenance::WroteIt)));
}

#[test]
fn after_a_failed_check_the_profile_is_unchanged_until_the_user_switches_to_1() {
    let dir = project_with_a_profile();
    let client = stops_at_a_boundary_then(vec![Ok(reply("guidance")), Ok(reply("done"))]);

    let (code, _) = run_in(
        &dir,
        &client,
        &[false],
        vec![vec![("src/main.rs", WITH_DBG)]],
        [
            input("debug 1"),
            Step::Select(3),
            input("/done"),
            Step::Interrupt,
            input("/abort"),
            input("/quit"),
        ],
    );
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(saved(&dir), None, "nothing saved after the failed check");

    let dir = project_with_a_profile();
    let client = stops_at_a_boundary_then(vec![
        Ok(reply("guidance")),
        Ok(ChatResponse {
            content: None,
            tool_calls: Vec::new(),
        }),
    ]);
    run_in(
        &dir,
        &client,
        &[false, true],
        vec![vec![("src/main.rs", WITH_DBG)]],
        [
            input("debug 1"),
            Step::Select(3),
            input("/done"),
            Step::Select(1),
            input("/quit"),
        ],
    );
    assert_eq!(saved(&dir), Some((Status::Known, Provenance::SideLine)));
}

#[test]
fn done_without_writing_reports_no_change_and_the_user_chooses_again() {
    let dir = project_with_a_profile();
    let client = stops_at_a_boundary_then(vec![
        Ok(reply("guidance")),
        Ok(write_main("third")),
        Ok(reply("done")),
    ]);

    let (code, ui) = run_in(
        &dir,
        &client,
        &[true],
        Vec::new(),
        [
            input("debug 1"),
            Step::Select(3),
            input("/done"),
            Step::Select(1),
            input("/quit"),
        ],
    );

    assert_eq!(code, ExitCode::SUCCESS);
    shown_at(
        &ui,
        &format!(
            "  No change: no .rs file or Cargo.toml was written since molt took the step back. {HINT}"
        ),
    );
    assert_eq!(saved(&dir), Some((Status::Known, Provenance::SideLine)));
}

#[test]
fn done_before_choosing_3_says_to_choose_it_first() {
    let dir = project_with_a_profile();
    let client = stops_at_a_boundary_then(Vec::new());

    let (_, ui) = run_in(
        &dir,
        &client,
        &[],
        vec![vec![("src/main.rs", WITH_DBG)]],
        [
            input("debug 1"),
            input("/done"),
            input("/abort"),
            input("/quit"),
        ],
    );

    shown_at(
        &ui,
        "  Choose ③ to write it yourself first, then type /done when you wrote it.",
    );
    assert_eq!(saved(&dir), None);
}

#[test]
fn ctrl_c_while_waiting_for_the_review_pauses_at_the_boundary_and_side_goes_on_writing() {
    let dir = project_with_a_profile();
    let client = stops_at_a_boundary_then(vec![
        Ok(reply("guidance")),
        Err(ChatError::Interrupted),
        Ok(reply("review")),
        Ok(reply("done")),
    ]);

    let (code, ui) = run_in(
        &dir,
        &client,
        &[true, true],
        vec![vec![("src/main.rs", WITH_DBG)]],
        [
            input("debug 1"),
            Step::Select(3),
            input("/done"),
            input("/side"),
            input("/done"),
            input("/quit"),
        ],
    );

    assert_eq!(code, ExitCode::SUCCESS);
    let paused = shown_at(&ui, PAUSED);
    assert_eq!(saved(&dir), Some((Status::Known, Provenance::WroteIt)));
    assert_eq!(
        ui.shown()[paused + 2],
        format!("  {WRITING}"),
        "{:?}",
        ui.shown()
    );
}

#[test]
fn a_guidance_that_cannot_be_got_is_shown_and_the_user_still_writes() {
    let dir = project_with_a_profile();
    let client = stops_at_a_boundary_then(vec![
        Err(ChatError::FakeExhausted),
        Ok(reply("review")),
        Ok(reply("done")),
    ]);

    let (code, ui) = run_in(
        &dir,
        &client,
        &[true],
        vec![vec![("src/main.rs", WITH_DBG)]],
        [
            input("debug 1"),
            Step::Select(3),
            input("/done"),
            input("/quit"),
        ],
    );

    assert_eq!(code, ExitCode::SUCCESS);
    let at = ui
        .shown()
        .iter()
        .position(|text| text.starts_with("  Could not get the guidance on where to write it: "))
        .unwrap_or_else(|| panic!("{:?}", ui.shown()));
    assert_eq!(ui.shown()[at + 1], format!("  {WRITING}"));
    assert_eq!(saved(&dir), Some((Status::Known, Provenance::WroteIt)));
}

#[test]
fn ctrl_c_while_waiting_for_the_guidance_pauses_at_the_boundary_without_writing() {
    let dir = project_with_a_profile();
    let client = stops_at_a_boundary_then(vec![Err(ChatError::Interrupted)]);

    let (code, ui) = run_in(
        &dir,
        &client,
        &[],
        vec![vec![("src/main.rs", WITH_DBG)]],
        [
            input("debug 1"),
            Step::Select(3),
            input("/side"),
            input("/done"),
            input("/abort"),
            input("/quit"),
        ],
    );

    assert_eq!(code, ExitCode::SUCCESS);
    let paused = shown_at(&ui, PAUSED);
    assert_eq!(ui.shown()[paused + 2], format!("  {HINT}"));
    shown_at(
        &ui,
        "  Choose ③ to write it yourself first, then type /done when you wrote it.",
    );
    assert_eq!(saved(&dir), None);
}

#[test]
fn a_review_that_cannot_be_got_is_shown_and_done_still_passes() {
    let dir = project_with_a_profile();
    let client = stops_at_a_boundary_then(vec![
        Ok(reply("guidance")),
        Err(ChatError::FakeExhausted),
        Ok(reply("done")),
    ]);

    let (code, ui) = run_in(
        &dir,
        &client,
        &[true],
        vec![vec![("src/main.rs", WITH_DBG)]],
        [
            input("debug 1"),
            Step::Select(3),
            input("/done"),
            input("/quit"),
        ],
    );

    assert_eq!(code, ExitCode::SUCCESS);
    let passed = shown_at(&ui, "  cargo check passed.");
    assert!(
        ui.shown()[passed + 1].starts_with("  Could not get the review: "),
        "{:?}",
        ui.shown()
    );
    assert_eq!(saved(&dir), Some((Status::Known, Provenance::WroteIt)));
}

#[test]
fn lines_with_no_item_new_to_the_profile_say_so_and_the_main_line_goes_on() {
    let dir = project_with_a_profile();
    let client = stops_at_a_boundary_then(vec![
        Ok(reply("guidance")),
        Ok(reply("review")),
        Ok(reply("done")),
    ]);

    let (code, ui) = run_in(
        &dir,
        &client,
        &[true],
        vec![vec![("src/main.rs", "fn main() {\n    let _x = 1;\n}\n")]],
        [
            input("debug 1"),
            Step::Select(3),
            input("/done"),
            input("/quit"),
        ],
    );

    assert_eq!(code, ExitCode::SUCCESS);
    let said = shown_at(
        &ui,
        "Your lines use nothing your profile did not have as known already.",
    );
    assert_eq!(ui.shown()[said + 1], "done");
    assert_eq!(saved(&dir), None);
}
