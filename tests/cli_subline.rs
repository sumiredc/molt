//! The side line (PRD UC3) through the binary without a terminal: the model is a chat API on a
//! loopback port, and the user's side comes from `--ui-script`, whose `"interrupt"` stands for
//! Ctrl-C.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::thread::{self, JoinHandle};

use assert_cmd::cargo::cargo_bin_cmd;
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
            requests.push(String::from_utf8(request).unwrap());
            let mut stream = stream;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
        requests
    });
    (base_url, handle)
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

#[test]
fn interrupt_on_the_side_line_pauses_the_main_line_at_the_boundary_and_changes_nothing() {
    // `dbg!` twice stops the request at a boundary, and the side line gets its explanation.
    let (base_url, server) = serve(vec![
        write_dbg("first"),
        write_dbg("second"),
        reply("dbg! prints a value and returns it."),
    ]);
    let dir = TempDir::new().unwrap();
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
    let paused = "Paused at the boundary. Type /side to go back to the side line, or /abort to withdraw the request.";
    let expected = [
        "molt> debug 1",
        "── side line: dbg_macro ────",
        "  dbg! prints a value and returns it.",
        "molt [sub: dbg_macro]> ^C",
        "── back to the main line ────",
        paused,
        "molt> /side",
        "── side line: dbg_macro ────",
        "molt [sub: dbg_macro]> ^C",
        "── back to the main line ────",
        paused,
        "molt> /abort",
        "Request withdrawn.",
        "molt> /quit",
    ];
    let mut rest = stdout.as_str();
    for line in expected {
        let at = rest
            .find(line)
            .unwrap_or_else(|| panic!("{line:?} not found in order in:\n{stdout}"));
        rest = &rest[at + line.len()..];
    }
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
