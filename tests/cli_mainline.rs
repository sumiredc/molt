//! `molt` without a subcommand (PRD §8.6, UC2's precondition and exception flow), driven through
//! the binary without a terminal: stdin is closed and the user's side comes from `--ui-script`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use assert_cmd::cargo::cargo_bin_cmd;
use tempfile::TempDir;

/// A directory with a profile, a ui script and a cargo project in it.
struct Setup {
    dir: TempDir,
}

impl Setup {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("profile.toml"), "").unwrap();
        let project = dir.path().join("project");
        fs::create_dir(&project).unwrap();
        fs::write(
            project.join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        Self { dir }
    }

    fn profile(&self) -> PathBuf {
        self.dir.path().join("profile.toml")
    }

    fn project(&self) -> PathBuf {
        self.dir.path().join("project")
    }

    fn script(&self, text: &str) -> PathBuf {
        let path = self.dir.path().join("ui.toml");
        fs::write(&path, text).unwrap();
        path
    }
}

/// Runs `molt --profile <profile> --ui-script <script>` in `cwd` with no molt directory in the
/// environment, so only `--profile` can decide where the profile is.
fn molt(cwd: &Path, profile: &Path, script: &Path) -> Output {
    cargo_bin_cmd!("molt")
        .current_dir(cwd)
        .env_remove("MOLT_HOME")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("HOME")
        .arg("--profile")
        .arg(profile)
        .arg("--ui-script")
        .arg(script)
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

#[test]
fn quit_from_a_ui_script_ends_with_exit_code_0_without_a_terminal() {
    let setup = Setup::new();
    let script = setup.script(r#"steps = [{ input = "/quit" }]"#);
    let output = molt(&setup.project(), &setup.profile(), &script);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(stdout(&output), "molt> /quit\n");
}

#[test]
fn other_inputs_are_taken_and_quit_still_ends_with_exit_code_0() {
    let setup = Setup::new();
    let script = setup
        .script(r#"steps = [{ input = "add a function that sums a slice" }, { input = "/quit" }]"#);
    let output = molt(&setup.project(), &setup.profile(), &script);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "molt> add a function that sums a slice\nmolt> /quit\n"
    );
}

#[test]
fn eof_from_a_ui_script_ends_with_exit_code_0() {
    let setup = Setup::new();
    let script = setup.script(r#"steps = ["eof"]"#);
    let output = molt(&setup.project(), &setup.profile(), &script);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
}

#[test]
fn interrupt_from_a_ui_script_ends_with_a_non_zero_exit_code() {
    let setup = Setup::new();
    let script = setup.script(r#"steps = ["interrupt"]"#);
    let output = molt(&setup.project(), &setup.profile(), &script);
    assert_eq!(
        output.status.code(),
        Some(130),
        "stderr: {}",
        stderr(&output)
    );
}

#[test]
fn without_a_profile_molt_prompts_for_molt_init_and_exits() {
    let setup = Setup::new();
    let missing = setup.dir.path().join("missing.toml");
    let script = setup.script(r#"steps = [{ input = "/quit" }]"#);
    let output = molt(&setup.project(), &missing, &script);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("run `molt init` to create one"),
        "{}",
        stderr(&output)
    );
    assert_eq!(stdout(&output), "", "the main line must not open");
    assert!(!missing.exists(), "no profile is created");
}

#[test]
fn without_a_profile_in_the_molt_directory_molt_prompts_for_molt_init_and_exits() {
    let setup = Setup::new();
    let home = setup.dir.path().join("molt-home");
    fs::create_dir(&home).unwrap();
    let script = setup.script(r#"steps = [{ input = "/quit" }]"#);
    let output = cargo_bin_cmd!("molt")
        .current_dir(setup.project())
        .env("MOLT_HOME", &home)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("HOME")
        .arg("--ui-script")
        .arg(&script)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains(&format!(
            "no profile at {}",
            home.join("profile.toml").display()
        )),
        "{}",
        stderr(&output)
    );
    assert!(stderr(&output).contains("run `molt init` to create one"));
    assert_eq!(stdout(&output), "", "the main line must not open");
}

#[test]
fn outside_a_cargo_project_molt_reports_it_and_stops() {
    let setup = Setup::new();
    let script = setup.script(r#"steps = [{ input = "/quit" }]"#);
    let output = molt(setup.dir.path(), &setup.profile(), &script);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("not a cargo project"),
        "{}",
        stderr(&output)
    );
    assert_eq!(stdout(&output), "", "the main line must not open");
}

#[test]
fn an_unreadable_ui_script_is_reported_before_the_main_line_opens() {
    let setup = Setup::new();
    let script = setup.script(r#"steps = [{ answer = "yes" }]"#);
    let output = molt(&setup.project(), &setup.profile(), &script);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("invalid ui script"),
        "{}",
        stderr(&output)
    );
    assert_eq!(stdout(&output), "");
}

#[test]
fn a_ui_script_that_runs_out_before_quit_ends_with_failure() {
    let setup = Setup::new();
    let script = setup.script(r#"steps = [{ input = "hello" }]"#);
    let output = molt(&setup.project(), &setup.profile(), &script);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("no step left"),
        "{}",
        stderr(&output)
    );
}
