//! `molt` without a subcommand (PRD §8.6, UC2's precondition and exception flow), driven through
//! the binary without a terminal: stdin is closed and the user's side comes from `--ui-script`.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::thread::{self, JoinHandle};

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
        .env_remove("MOLT_LOG")
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

/// The stdout of `output` before its last line, which shows where the session log is, as after
/// `/quit` or Ctrl-D.
fn before_session_path(output: &Output) -> String {
    let stdout = stdout(output);
    let last = stdout
        .trim_end_matches('\n')
        .rfind('\n')
        .map_or(0, |at| at + 1);
    assert!(
        stdout[last..].starts_with("Session log: "),
        "no session log path last: {stdout}"
    );
    stdout[..last].to_owned()
}

#[test]
fn quit_from_a_ui_script_ends_with_exit_code_0_without_a_terminal() {
    let setup = Setup::new();
    let script = setup.script(r#"steps = [{ input = "/quit" }]"#);
    let output = molt(&setup, &setup.project(), &setup.profile(), &script);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(before_session_path(&output), "molt> /quit\n");
}

#[test]
fn an_older_profile_with_allow_is_read_with_the_note_and_left_as_it_was() {
    let setup = Setup::new();
    let older = "allow = [\"arc\"]\n";
    fs::write(setup.profile(), older).unwrap();
    let script = setup.script(r#"steps = [{ input = "/quit" }]"#);
    let output = molt(&setup, &setup.project(), &setup.profile(), &script);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert!(
        stderr(&output).contains("arc was in `allow`"),
        "{}",
        stderr(&output)
    );
    assert_eq!(fs::read_to_string(setup.profile()).unwrap(), older);
}

#[test]
fn an_unreachable_api_is_reported_the_files_stay_and_the_prompt_comes_back() {
    let setup = Setup::new();
    let manifest = fs::read(setup.project().join("Cargo.toml")).unwrap();
    let script = setup
        .script(r#"steps = [{ input = "add a function that sums a slice" }, { input = "/quit" }]"#);
    let output = molt(&setup, &setup.project(), &setup.profile(), &script);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let stdout = before_session_path(&output);
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

/// Runs `molt --profile <profile> --ui-script <script>` with `fake` (`--fake` and what follows it)
/// in the project of `setup`. `config.toml` names an API key variable that is unset, so only a
/// fake client can open the main line.
fn molt_with_fake(setup: &Setup, script: &Path, fake: &[&std::ffi::OsStr]) -> Output {
    cargo_bin_cmd!("molt")
        .current_dir(setup.project())
        .env("MOLT_HOME", setup.home())
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("HOME")
        .env_remove("MOLT_LOG")
        .env_remove("MOLT_TEST_FAKE_SCRIPT_API_KEY")
        .arg("--profile")
        .arg(setup.profile())
        .arg("--ui-script")
        .arg(script)
        .args(fake)
        .output()
        .unwrap()
}

/// A setup whose `config.toml` needs an API key from a variable that is never set.
fn setup_without_a_key() -> Setup {
    Setup::with_api(&format!(
        "base_url = \"http://127.0.0.1:{}/v1\"\napi_key_env = \"MOLT_TEST_FAKE_SCRIPT_API_KEY\"\n",
        closed_port()
    ))
}

#[test]
fn a_fake_script_runs_a_turn_offline_with_its_responses() {
    let setup = setup_without_a_key();
    let fake = setup.dir.path().join("fake.toml");
    fs::write(
        &fake,
        r#"
[[responses]]
tool_calls = [
  { name = "write_file", arguments = { path = "src/lib.rs", content = "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n" } },
]

[[responses]]
tool_calls = [{ name = "finish", arguments = { summary = "Added `add` to src/lib.rs." } }]
"#,
    )
    .unwrap();
    let script = setup.script(r#"steps = [{ input = "add two numbers" }, { input = "/quit" }]"#);
    let output = molt_with_fake(&setup, &script, &["--fake".as_ref(), fake.as_os_str()]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));

    assert_eq!(
        fs::read_to_string(setup.project().join("src/lib.rs")).unwrap(),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n"
    );
    let stdout = before_session_path(&output);
    assert!(
        stdout.starts_with("molt> add two numbers\nAdded `add` to src/lib.rs.\n"),
        "{stdout}"
    );
    assert!(
        stdout.contains("+pub fn add(a: i32, b: i32) -> i32 {"),
        "{stdout}"
    );
    assert!(stdout.contains("cargo check passed."), "{stdout}");
    assert!(stdout.ends_with("molt> /quit\n"), "{stdout}");
}

#[test]
fn a_fake_script_that_runs_out_stops_the_turn_and_the_prompt_comes_back() {
    let setup = setup_without_a_key();
    let fake = setup.dir.path().join("fake.toml");
    fs::write(&fake, "responses = []\n").unwrap();
    let script = setup.script(r#"steps = [{ input = "hello" }, { input = "/quit" }]"#);
    let output = molt_with_fake(&setup, &script, &["--fake".as_ref(), fake.as_os_str()]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(
        before_session_path(&output),
        "molt> hello\nStopped: the fake chat client has no more queued responses\nmolt> /quit\n"
    );
}

#[test]
fn fake_without_a_fake_script_path_shows_the_usage_and_exits() {
    let setup = setup_without_a_key();
    let script = setup.script(r#"steps = [{ input = "/quit" }]"#);
    let output = molt_with_fake(&setup, &script, &["--fake".as_ref()]);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr(&output).contains("Usage: molt"),
        "{}",
        stderr(&output)
    );
    assert_eq!(stdout(&output), "", "the main line must not open");
}

#[test]
fn an_unreadable_fake_script_is_reported_before_the_main_line_opens() {
    let setup = setup_without_a_key();
    let missing = setup.dir.path().join("missing.toml");
    let script = setup.script(r#"steps = [{ input = "/quit" }]"#);
    let output = molt_with_fake(&setup, &script, &["--fake".as_ref(), missing.as_os_str()]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("cannot read the fake script"),
        "{}",
        stderr(&output)
    );
    assert_eq!(stdout(&output), "", "the main line must not open");
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

/// The files in the session log directory of `setup`, by name.
fn session_files(setup: &Setup) -> Vec<(String, String)> {
    let mut files: Vec<(String, String)> = fs::read_dir(setup.home().join("sessions"))
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_str().unwrap().to_owned();
            (name, fs::read_to_string(&path).unwrap())
        })
        .collect();
    files.sort();
    files
}

#[test]
fn the_session_log_is_one_jsonl_file_in_the_molt_directory_and_without_molt_log_nothing_else() {
    let setup = Setup::new();
    let script = setup.script(r#"steps = [{ input = "hello" }, { input = "/quit" }]"#);
    let output = molt(&setup, &setup.project(), &setup.profile(), &script);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));

    let files = session_files(&setup);
    assert_eq!(files.len(), 1, "{files:?}");
    assert!(files[0].0.ends_with(".jsonl"), "{files:?}");
    assert!(!files[0].0.ends_with(".requests.jsonl"), "{files:?}");
    assert_eq!(fs::read_dir(setup.project()).unwrap().count(), 1);
}

#[test]
fn session_path_quit_prints_where_the_session_log_is_on_stdout() {
    let setup = Setup::new();
    let script = setup.script(r#"steps = [{ input = "/quit" }]"#);
    let output = molt(&setup, &setup.project(), &setup.profile(), &script);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));

    let files = session_files(&setup);
    assert_eq!(files.len(), 1, "{files:?}");
    let path = setup.home().join("sessions").join(&files[0].0);
    assert!(
        stdout(&output).ends_with(&format!("Session log: {}\n", path.display())),
        "{}",
        stdout(&output)
    );
}

#[test]
fn when_the_session_log_cannot_be_created_the_main_line_does_not_open() {
    let setup = Setup::new();
    // A file where the directory of the session logs would be.
    fs::write(setup.home().join("sessions"), "").unwrap();
    let script = setup.script(r#"steps = [{ input = "/quit" }]"#);
    let output = molt(&setup, &setup.project(), &setup.profile(), &script);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("cannot create the session log in "),
        "{}",
        stderr(&output)
    );
    assert_eq!(stdout(&output), "");
}

/// A chat API on a loopback port that answers each request with the next of `responses`
/// (status, body), and hands back the `authorization` header of each request.
fn serve(responses: Vec<(u16, String)>) -> (String, JoinHandle<Vec<Option<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let mut authorizations = Vec::new();
        for (status, body) in responses {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let (mut length, mut authorization) = (0, None);
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let line = line.trim_end();
                if line.is_empty() {
                    break;
                }
                let (key, value) = line.split_once(':').unwrap();
                match key.trim().to_lowercase().as_str() {
                    "content-length" => length = value.trim().parse().unwrap(),
                    "authorization" => authorization = Some(value.trim().to_owned()),
                    _ => {}
                }
            }
            reader.read_exact(&mut vec![0; length]).unwrap();
            let mut stream = stream;
            write!(
                stream,
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            authorizations.push(authorization);
        }
        authorizations
    });
    (base_url, handle)
}

/// A response of the chat API with a `write_file` call of `content` to `src/main.rs`.
fn write_main(id: &str, content: &str) -> (u16, String) {
    let arguments = serde_json::json!({ "path": "src/main.rs", "content": content }).to_string();
    let body = serde_json::json!({
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": id,
                    "type": "function",
                    "function": { "name": "write_file", "arguments": arguments },
                }],
            },
            "finish_reason": "tool_calls",
        }],
    });
    (200, body.to_string())
}

/// A response of the chat API with `text` and no tool call.
fn reply(text: &str) -> (u16, String) {
    let body = serde_json::json!({
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": text },
            "finish_reason": "stop",
        }],
    });
    (200, body.to_string())
}

#[test]
fn the_api_key_appears_nowhere_in_the_session_log_the_request_bodies_or_stdout() {
    const KEY: &str = "sk-molt-session-test-4f9c2e";
    // `dbg!` twice stops the first request at a boundary. The model writes the key back in the
    // content of both, which the log records as the arguments of the tool call and the rejected
    // content. The side line gets its explanation. The next request is refused by an endpoint
    // that writes the key back in its body.
    let with_dbg = format!("fn main() {{\n    dbg!(\"{KEY}\");\n}}\n");
    let (base_url, server) = serve(vec![
        write_main("first", &with_dbg),
        write_main("second", &with_dbg),
        reply("dbg! prints a value."),
        (401, format!(r#"{{"error":"invalid api key {KEY}"}}"#)),
    ]);
    let setup = Setup::with_api(&format!(
        "base_url = \"{base_url}\"\napi_key_env = \"MOLT_TEST_SESSION_API_KEY\"\n"
    ));
    let script = setup.script(
        r#"steps = [{ input = "debug 1" }, { input = "/abort" }, { input = "next" }, { input = "/quit" }]"#,
    );
    let output = cargo_bin_cmd!("molt")
        .current_dir(setup.project())
        .env("MOLT_HOME", setup.home())
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("HOME")
        .env("MOLT_TEST_SESSION_API_KEY", KEY)
        .env("MOLT_LOG", "1")
        .arg("--profile")
        .arg(setup.profile())
        .arg("--ui-script")
        .arg(&script)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));

    // The key was in play: every request carried it.
    let authorizations = server.join().unwrap();
    assert_eq!(authorizations, vec![Some(format!("Bearer {KEY}")); 4]);
    let stdout = stdout(&output);
    assert!(stdout.contains("Stopped at a boundary"), "{stdout}");
    assert!(stdout.contains("HTTP 401"), "{stdout}");
    assert!(!stdout.contains(KEY), "{stdout}");
    assert!(!stderr(&output).contains(KEY));
    let files = session_files(&setup);
    let names: Vec<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names.len(), 2, "{names:?}");
    assert!(names[0].ends_with(".jsonl") && !names[0].ends_with(".requests.jsonl"));
    assert!(names[1].ends_with(".requests.jsonl"), "{names:?}");
    let (events, requests) = (&files[0].1, &files[1].1);
    assert!(events.contains(r#""event":"rejected""#), "{events}");
    // What the model wrote back is in the log, with the key hidden.
    assert!(events.contains(r#"dbg!(\"<redacted>\")"#), "{events}");
    // The two writes, the side line's explanation and the next request.
    assert_eq!(requests.lines().count(), 4, "{requests}");
    for (name, content) in &files {
        assert!(!content.contains(KEY), "{name}: {content}");
    }
}
