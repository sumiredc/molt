//! The side line (PRD UC3) through the binary without a terminal: the model is a chat API on a
//! loopback port, and the user's side comes from `--ui-script`, whose `"interrupt"` stands for
//! Ctrl-C at a prompt. Ctrl-C while waiting for the model is a real SIGINT sent to the process.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::thread::{self, JoinHandle};

use assert_cmd::cargo::cargo_bin_cmd;
use molt::profile::status::{Provenance, Status};
use molt::profile::store::Profile;
use tempfile::TempDir;

/// A chat API on a loopback port that answers each request with the next of `bodies` (status
/// 200), and hands back the body of each request.
fn serve(bodies: Vec<String>) -> (String, JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let mut requests = Vec::new();
        for body in bodies {
            let (stream, _) = listener.accept().unwrap();
            requests.push(read_request(&stream));
            respond(stream, &body);
        }
        requests
    });
    (base_url, handle)
}

/// Reads one HTTP request from `stream` and returns its body.
fn read_request(stream: &TcpStream) -> String {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let mut length = 0;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        let (key, value) = line.split_once(':').unwrap();
        if key.trim().eq_ignore_ascii_case("content-length") {
            length = value.trim().parse().unwrap();
        }
    }
    let mut request = vec![0; length];
    reader.read_exact(&mut request).unwrap();
    String::from_utf8(request).unwrap()
}

/// Answers with `body` (status 200) and closes the connection.
fn respond(mut stream: TcpStream, body: &str) {
    write!(
        stream,
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
}

/// A response with a `write_file` call that writes `dbg!` to `src/main.rs`.
fn write_dbg(id: &str) -> String {
    let arguments = serde_json::json!({
        "path": "src/main.rs",
        "content": "fn main() {\n    dbg!(1);\n}\n",
    })
    .to_string();
    serde_json::json!({
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
    })
    .to_string()
}

/// A response with `text` and no tool call.
fn reply(text: &str) -> String {
    serde_json::json!({
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": text },
            "finish_reason": "stop",
        }],
    })
    .to_string()
}

/// In `dir`: molt's place with a `config.toml` pointing at `base_url`, an empty `profile.toml`,
/// and a cargo project with `src/main.rs`. Returns the three paths.
fn setup(dir: &TempDir, base_url: &str) -> (PathBuf, PathBuf, PathBuf) {
    let home = dir.path().join("molt-home");
    fs::create_dir(&home).unwrap();
    fs::write(
        home.join("config.toml"),
        format!("[api]\nbase_url = \"{base_url}\"\napi_key_env = \"\"\n"),
    )
    .unwrap();
    let profile = dir.path().join("profile.toml");
    fs::write(&profile, "").unwrap();
    let project = dir.path().join("project");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("Cargo.toml"), "[package]\nname = \"demo\"\n").unwrap();
    fs::write(project.join("src/main.rs"), "fn main() {}\n").unwrap();
    (home, profile, project)
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

const PAUSED: &str = "Paused at the boundary. Type /side to go back to the side line, or /abort to withdraw the request.";

#[test]
fn interrupt_on_the_side_line_pauses_the_main_line_at_the_boundary_and_changes_nothing() {
    // `dbg!` twice stops the request at a boundary, and the side line gets its explanation.
    let (base_url, server) = serve(vec![
        write_dbg("first"),
        write_dbg("second"),
        reply("dbg! prints a value and returns it."),
    ]);
    let dir = TempDir::new().unwrap();
    let (home, profile, project) = setup(&dir, &base_url);
    let script = dir.path().join("ui.toml");
    fs::write(
        &script,
        r#"steps = [{ input = "debug 1" }, "interrupt", { input = "/side" }, "interrupt", { input = "/abort" }, { input = "/quit" }]"#,
    )
    .unwrap();

    let output = cargo_bin_cmd!("molt")
        .current_dir(&project)
        .env("MOLT_HOME", &home)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("HOME")
        .env_remove("MOLT_LOG")
        .arg("--profile")
        .arg(&profile)
        .arg("--ui-script")
        .arg(&script)
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_in_order(
        &stdout,
        &[
            "molt> debug 1",
            "── side line: dbg_macro ────",
            "  dbg! prints a value and returns it.",
            "molt [sub: dbg_macro]> ^C",
            "── back to the main line ────",
            PAUSED,
            "molt> /side",
            "── side line: dbg_macro ────",
            "molt [sub: dbg_macro]> ^C",
            "── back to the main line ────",
            PAUSED,
            "molt> /abort",
            "Request withdrawn.",
            "molt> /quit",
        ],
    );
    let requests = server.join().unwrap();
    assert_eq!(
        requests.len(),
        3,
        "the side line was opened again without a request"
    );
    assert_eq!(
        fs::read_to_string(project.join("src/main.rs")).unwrap(),
        "fn main() {}\n"
    );
    assert_eq!(fs::read_to_string(&profile).unwrap(), "");
}

/// Ctrl-C (a real SIGINT) while the side line waits for the explanation: molt does not end, the
/// main line is paused at the boundary, and `/side` asks for the explanation again.
#[cfg(unix)]
#[test]
fn sigint_while_waiting_for_the_explanation_pauses_the_main_line_at_the_boundary() {
    use std::process::{Command, Stdio};
    use std::sync::mpsc;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let (waiting, explanation_asked) = mpsc::channel();
    let server = thread::spawn(move || {
        let mut requests = Vec::new();
        let mut held = Vec::new();
        let bodies = [
            Some(write_dbg("first")),
            Some(write_dbg("second")),
            None,
            Some(reply("dbg! prints a value and returns it.")),
        ];
        for body in bodies {
            let (stream, _) = listener.accept().unwrap();
            requests.push(read_request(&stream));
            match body {
                Some(body) => respond(stream, &body),
                // No answer: molt is now waiting for the explanation.
                None => {
                    held.push(stream);
                    waiting.send(()).unwrap();
                }
            }
        }
        requests
    });
    let dir = TempDir::new().unwrap();
    let (home, profile, project) = setup(&dir, &base_url);
    let script = dir.path().join("ui.toml");
    fs::write(
        &script,
        r#"steps = [{ input = "debug 1" }, { input = "/side" }, { input = "/abort" }, { input = "/quit" }]"#,
    )
    .unwrap();

    let child = Command::new(env!("CARGO_BIN_EXE_molt"))
        .current_dir(&project)
        .env("MOLT_HOME", &home)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("HOME")
        .env_remove("MOLT_LOG")
        .arg("--profile")
        .arg(&profile)
        .arg("--ui-script")
        .arg(&script)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    explanation_asked.recv().unwrap();
    let killed = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    let output = child.wait_with_output().unwrap();

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
            "molt> debug 1",
            "── side line: dbg_macro ────",
            "── back to the main line ────",
            PAUSED,
            "molt> /side",
            "── side line: dbg_macro ────",
            "  dbg! prints a value and returns it.",
            "molt [sub: dbg_macro]> /abort",
            "Request withdrawn.",
            "molt> /quit",
        ],
    );
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 4, "the explanation is asked for again");
    assert_eq!(requests[3], requests[2]);
    assert_eq!(
        fs::read_to_string(project.join("src/main.rs")).unwrap(),
        "fn main() {}\n"
    );
    assert_eq!(fs::read_to_string(&profile).unwrap(), "");
}

/// A response with a `write_file` call that writes `content` to `src/main.rs`.
fn write_main(id: &str, content: &str) -> String {
    let arguments = serde_json::json!({ "path": "src/main.rs", "content": content }).to_string();
    serde_json::json!({
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
    })
    .to_string()
}

const WITH_MATCHES: &str = "fn main() {\n    let _ = matches!(1, 1);\n}\n";
const WITH_CLONE: &str = "fn main() {\n    let s = String::new();\n    let _t = s.clone();\n}\n";

/// The lines of the session log in `dir`, or of its request bodies when `requests` is true.
fn session_log(dir: &std::path::Path, requests: bool) -> Vec<serde_json::Value> {
    let path = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            let name = path.file_name().unwrap().to_str().unwrap();
            name.ends_with(".requests.jsonl") == requests
        })
        .unwrap();
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// Three boundaries in a session, with ① ② ③ chosen at them in that order through `--ui-script`:
/// the §8.5 message of each enters the main line, the profile changes as each choice says, and
/// the session log has the choice and the profile change of each with `line: "sub"`.
#[test]
fn unlock_rewrite_and_user_wrote_at_three_boundaries_each_hand_the_main_line_their_message() {
    let dir = TempDir::new().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let (home, profile, project) = setup(&dir, &base_url);
    let main_rs = project.join("src/main.rs");
    // Each answer, and with ③'s guidance, what the user writes before typing `/done`.
    let answers: Vec<(String, Option<&str>)> = vec![
        // ① at `dbg_macro`.
        (write_dbg("d1"), None),
        (write_dbg("d2"), None),
        (reply("dbg! prints a value and returns it."), None),
        (write_dbg("d3"), None),
        (reply("Printed 1 with dbg!."), None),
        // ② at `matches_macro`.
        (write_main("m1", WITH_MATCHES), None),
        (write_main("m2", WITH_MATCHES), None),
        (
            reply("matches! tells whether a value fits a pattern."),
            None,
        ),
        (reply("Compare with == instead."), None),
        (reply("Rewrote it with ==."), None),
        // ③ at `clone`.
        (write_main("c1", WITH_CLONE), None),
        (write_main("c2", WITH_CLONE), None),
        (reply("clone() copies a value."), None),
        (reply("Write the copy in src/main.rs."), Some(WITH_CLONE)),
        (reply("The copy is right."), None),
        (reply("Kept your copy."), None),
    ];
    let server = thread::spawn(move || {
        let mut requests = Vec::new();
        for (body, written) in answers {
            let (stream, _) = listener.accept().unwrap();
            requests.push(read_request(&stream));
            if let Some(content) = written {
                fs::write(&main_rs, content).unwrap();
            }
            respond(stream, &body);
        }
        requests
    });
    let script = dir.path().join("ui.toml");
    fs::write(
        &script,
        r#"steps = [
  { input = "debug 1" }, { input = "/choose" }, { select = 1 },
  { input = "match 1" }, { input = "/choose" }, { select = 2 },
  { input = "copy a string" }, { input = "/choose" }, { select = 3 }, { input = "/done" },
  { input = "/quit" },
]"#,
    )
    .unwrap();

    let output = cargo_bin_cmd!("molt")
        .current_dir(&project)
        .env("MOLT_HOME", &home)
        .env("MOLT_LOG", "1")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("HOME")
        .arg("--profile")
        .arg(&profile)
        .arg("--ui-script")
        .arg(&script)
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
            "molt> debug 1",
            "── side line: dbg_macro ────",
            "molt [sub: dbg_macro]> /choose",
            "Your profile allows dbg_macro now (known).",
            "Printed 1 with dbg!.",
            "molt> match 1",
            "── side line: matches_macro ────",
            "molt [sub: matches_macro]> /choose",
            "Your profile marks matches_macro as seen, and molt rewrites without them. Policy: Compare with == instead.",
            "Rewrote it with ==.",
            "molt> copy a string",
            "── side line: clone ────",
            "molt [sub: clone]> /choose",
            "  Write the copy in src/main.rs.",
            "molt [sub: clone]> /done",
            "  The copy is right.",
            "Your profile allows clone now (known: you wrote it).",
            "Kept your copy.",
            "molt> /quit",
        ],
    );

    // The message of each choice is the last user message of the main line's next request.
    let requests: Vec<serde_json::Value> = server
        .join()
        .unwrap()
        .iter()
        .map(|body| serde_json::from_str(body).unwrap())
        .collect();
    let last_user = |at: usize| -> String {
        requests[at]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .find(|message| message["role"] == "user")
            .unwrap()["content"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let unlock = last_user(3);
    assert!(
        unlock.starts_with("BOUNDARY RESOLVED: unlock\n"),
        "{unlock}"
    );
    assert!(unlock.contains("`dbg_macro`"), "{unlock}");
    let rewrite = last_user(9);
    assert!(
        rewrite.starts_with("BOUNDARY RESOLVED: rewrite\n"),
        "{rewrite}"
    );
    assert!(rewrite.contains("`matches_macro`"), "{rewrite}");
    assert!(
        rewrite.contains("Policy: Compare with == instead."),
        "{rewrite}"
    );
    let user_wrote = last_user(15);
    assert!(
        user_wrote.starts_with("BOUNDARY RESOLVED: user_wrote\n"),
        "{user_wrote}"
    );
    assert!(user_wrote.contains("`clone`"), "{user_wrote}");
    assert!(user_wrote.contains("`src/main.rs`"), "{user_wrote}");

    let profile = Profile::load(&profile).unwrap();
    for (item, status, provenance) in [
        ("dbg_macro", Status::Known, Provenance::SideLine),
        ("matches_macro", Status::Seen, Provenance::SideLine),
        ("clone", Status::Known, Provenance::WroteIt),
    ] {
        let entry = profile.entry(&item.parse().unwrap()).unwrap();
        assert_eq!(
            (item, entry.status(), entry.provenance()),
            (item, status, provenance)
        );
    }

    // The session log: the side line's events carry `line: "sub"`, and the main line's do not.
    let sessions = home.join("sessions");
    let events = session_log(&sessions, false);
    let sub: Vec<(&str, &str)> = events
        .iter()
        .filter(|event| event.get("line").is_some())
        .map(|event| {
            assert_eq!(event["line"], "sub", "{event}");
            let what = event["choice"].as_str().or(event["item"].as_str());
            (event["event"].as_str().unwrap(), what.unwrap())
        })
        .collect();
    assert_eq!(
        sub,
        [
            ("choice", "unlock"),
            ("profile_change", "dbg_macro"),
            ("choice", "rewrite"),
            ("profile_change", "matches_macro"),
            ("choice", "user_wrote"),
            ("profile_change", "clone"),
        ]
    );
    for event in events.iter().filter(|event| event.get("line").is_none()) {
        let kind = event["event"].as_str().unwrap();
        assert!(
            ["tool_call", "detected", "rejected", "cargo_check"].contains(&kind),
            "{event}"
        );
    }
    // The request bodies: those of the side line carry `line: "sub"` too.
    let bodies = session_log(&sessions, true);
    assert_eq!(bodies.len(), 16);
    let side: Vec<usize> = (0..bodies.len())
        .filter(|&at| bodies[at].get("line").is_some_and(|line| line == "sub"))
        .collect();
    assert_eq!(
        side,
        [2, 7, 8, 12, 13, 14],
        "the explanations, the policy of ②, the guidance and the review of ③"
    );
}
