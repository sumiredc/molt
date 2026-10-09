//! `molt` without a subcommand (PRD §8.6, UC2's precondition and exception flow), driven through
//! the binary without a terminal: stdin is closed and the user's side comes from `--ui-script`.

use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Output;

use assert_cmd::cargo::cargo_bin_cmd;
use tempfile::TempDir;

/// A directory with a profile, a ui script, a molt directory and a cargo project in it.
///
/// The molt directory's `config.toml` points at a loopback port nothing listens on, so a
/// request to the model fails without leaving the machine.
struct Setup {
    dir: TempDir,
}

/// A loopback port nothing listens on: bound by the OS, then released.
fn closed_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

impl Setup {
    fn new() -> Self {
        Self::with_api(&format!(
            "base_url = \"http://127.0.0.1:{}/v1\"\napi_key_env = \"\"\n",
            closed_port()
        ))
    }

    /// `api` is the body of the `[api]` table in `config.toml`.
    fn with_api(api: &str) -> Self {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("profile.toml"), "").unwrap();
        let home = dir.path().join("molt-home");
        fs::create_dir(&home).unwrap();
        fs::write(home.join("config.toml"), format!("[api]\n{api}")).unwrap();
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

    fn home(&self) -> PathBuf {
        self.dir.path().join("molt-home")
    }

    fn script(&self, text: &str) -> PathBuf {
        let path = self.dir.path().join("ui.toml");
        fs::write(&path, text).unwrap();
        path
    }
}

/// Runs `molt --profile <profile> --ui-script <script>` in `cwd`, with the molt directory of
/// `setup` for `config.toml`. Its molt directory has no profile, so only `--profile` can decide
/// where the profile is.
fn molt(setup: &Setup, cwd: &Path, profile: &Path, script: &Path) -> Output {
    cargo_bin_cmd!("molt")
        .current_dir(cwd)
        .env("MOLT_HOME", setup.home())
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
    let output = molt(&setup, &setup.project(), &setup.profile(), &script);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(stdout(&output), "molt> /quit\n");
}

#[test]
fn an_unreachable_api_is_reported_the_files_stay_and_the_prompt_comes_back() {
    let setup = Setup::new();
    let manifest = fs::read(setup.project().join("Cargo.toml")).unwrap();
    let script = setup
        .script(r#"steps = [{ input = "add a function that sums a slice" }, { input = "/quit" }]"#);
    let output = molt(&setup, &setup.project(), &setup.profile(), &script);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let stdout = stdout(&output);
    assert!(
        stdout.starts_with(
            "molt> add a function that sums a slice\nStopped: request to the chat API failed: "
        ),
        "{stdout}"
    );
    assert!(stdout.ends_with("molt> /quit\n"), "{stdout}");
    assert_eq!(
        fs::read(setup.project().join("Cargo.toml")).unwrap(),
        manifest
    );
    assert_eq!(fs::read_dir(setup.project()).unwrap().count(), 1);
}

#[test]
fn without_the_api_key_molt_reports_its_variable_and_does_not_open_the_main_line() {
    let setup = Setup::with_api("api_key_env = \"MOLT_TEST_MISSING_API_KEY\"\n");
    let script = setup.script(r#"steps = [{ input = "/quit" }]"#);
    let output = cargo_bin_cmd!("molt")
        .current_dir(setup.project())
        .env("MOLT_HOME", setup.home())
        .env_remove("MOLT_TEST_MISSING_API_KEY")
        .arg("--profile")
        .arg(setup.profile())
        .arg("--ui-script")
        .arg(&script)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("`MOLT_TEST_MISSING_API_KEY` is not set"),
        "{}",
        stderr(&output)
    );
    assert_eq!(stdout(&output), "", "the main line must not open");
}

#[test]
fn the_main_line_refuses_fake_until_it_can_read_the_script() {
    let setup = Setup::new();
    let script = setup.script(r#"steps = [{ input = "/quit" }]"#);
    let output = cargo_bin_cmd!("molt")
        .current_dir(setup.project())
        .env("MOLT_HOME", setup.home())
        .arg("--profile")
        .arg(setup.profile())
        .arg("--ui-script")
        .arg(&script)
        .arg("--fake")
        .arg(&script)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("does not support --fake yet"),
        "{}",
        stderr(&output)
    );
    assert_eq!(stdout(&output), "");
}

#[test]
fn eof_from_a_ui_script_ends_with_exit_code_0() {
    let setup = Setup::new();
    let script = setup.script(r#"steps = ["eof"]"#);
    let output = molt(&setup, &setup.project(), &setup.profile(), &script);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
}

#[test]
fn interrupt_from_a_ui_script_ends_with_a_non_zero_exit_code() {
    let setup = Setup::new();
    let script = setup.script(r#"steps = ["interrupt"]"#);
    let output = molt(&setup, &setup.project(), &setup.profile(), &script);
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
    let output = molt(&setup, &setup.project(), &missing, &script);
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
    let home = setup.home();
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
    let output = molt(&setup, setup.dir.path(), &setup.profile(), &script);
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
    let output = molt(&setup, &setup.project(), &setup.profile(), &script);
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
    let output = molt(&setup, &setup.project(), &setup.profile(), &script);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("no step left"),
        "{}",
        stderr(&output)
    );
}
