//! The script file of `--fake PATH` (PRD §8.6): the model's responses in order, loaded into a
//! [`FakeChatClient`] so the main line runs offline, without a model.
//!
//! The file is toml with one array of tables, `responses`. Each answers the next request, in
//! order. A response has a text (`content`), tool calls (`tool_calls`), or both:
//!
//! ```toml
//! [[responses]]
//! content = "I'll add `add` to src/lib.rs."
//! tool_calls = [
//!   { name = "write_file", arguments = { path = "src/lib.rs", content = "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n" } },
//! ]
//!
//! [[responses]]
//! tool_calls = [{ name = "finish", arguments = { summary = "Added `add`." } }]
//! ```
//!
//! `arguments` is a table, sent to molt as its JSON, or a string sent as it is (to script
//! arguments that are not valid JSON). Without it the arguments are `{}`. A tool call's `id` is
//! `call_1`, `call_2`, … in the order of the file, unless it is given. When the responses run
//! out, the next request fails as an API error would.

use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::FakeChatClient;
use super::types::{ChatResponse, ToolCall};

/// The shape of a script file.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScriptFile {
    responses: Vec<ScriptResponse>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScriptResponse {
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ScriptToolCall>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScriptToolCall {
    id: Option<String>,
    name: String,
    arguments: Option<Arguments>,
}

/// A tool call's arguments as written in the script.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Arguments {
    /// Sent unchanged, so a script can send arguments that are not valid JSON.
    Raw(String),
    /// Sent as its JSON.
    Table(toml::Table),
}

impl Arguments {
    fn into_json(self) -> String {
        match self {
            Arguments::Raw(text) => text,
            Arguments::Table(table) => {
                serde_json::to_string(&table).expect("a toml table is always representable as JSON")
            }
        }
    }
}

/// Why a script file could not be read.
#[derive(Debug)]
pub(crate) enum FakeScriptError {
    Io {
        path: PathBuf,
        source: io::Error,
    },
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
}

impl fmt::Display for FakeScriptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FakeScriptError::Io { path, source } => {
                write!(
                    f,
                    "cannot read the fake script {}: {source}",
                    path.display()
                )
            }
            FakeScriptError::Parse { path, source } => {
                write!(f, "invalid fake script {}: {source}", path.display())
            }
        }
    }
}

impl Error for FakeScriptError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            FakeScriptError::Io { source, .. } => Some(source),
            FakeScriptError::Parse { source, .. } => Some(source),
        }
    }
}

/// Reads the script file at `path` (the format is in the module documentation) and returns a
/// [`FakeChatClient`] with its responses queued in order.
pub(crate) fn load(path: &Path) -> Result<FakeChatClient, FakeScriptError> {
    let text = fs::read_to_string(path).map_err(|source| FakeScriptError::Io {
        path: path.to_owned(),
        source,
    })?;
    let responses = parse(&text).map_err(|source| FakeScriptError::Parse {
        path: path.to_owned(),
        source,
    })?;
    let client = FakeChatClient::new();
    for response in responses {
        client.push_chat(Ok(response));
    }
    Ok(client)
}

/// The responses of a script, in order.
fn parse(text: &str) -> Result<Vec<ChatResponse>, toml::de::Error> {
    let file: ScriptFile = toml::from_str(text)?;
    let mut numbered = 0;
    Ok(file
        .responses
        .into_iter()
        .map(|response| ChatResponse {
            content: response.content,
            tool_calls: response
                .tool_calls
                .into_iter()
                .map(|call| {
                    numbered += 1;
                    ToolCall {
                        id: call.id.unwrap_or_else(|| format!("call_{numbered}")),
                        name: call.name,
                        arguments: call
                            .arguments
                            .map_or_else(|| "{}".to_owned(), Arguments::into_json),
                    }
                })
                .collect(),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::ChatClient;
    use crate::llm::types::{ChatError, ChatRequest, Message};

    fn request() -> ChatRequest {
        ChatRequest {
            model: "model".to_owned(),
            messages: vec![Message::User {
                content: "hi".to_owned(),
            }],
            tools: Vec::new(),
            tool_choice: None,
            response_format: None,
            reasoning_effort: None,
        }
    }

    fn call(id: &str, name: &str, arguments: &str) -> ToolCall {
        ToolCall {
            id: id.to_owned(),
            name: name.to_owned(),
            arguments: arguments.to_owned(),
        }
    }

    #[test]
    fn a_text_response_is_returned_as_its_content() {
        let responses = parse("[[responses]]\ncontent = \"hello\"\n").unwrap();
        assert_eq!(
            responses,
            [ChatResponse {
                content: Some("hello".to_owned()),
                tool_calls: Vec::new(),
            }]
        );
    }

    #[test]
    fn tool_calls_take_a_table_of_arguments_as_its_json() {
        let responses = parse(
            r#"
            [[responses]]
            tool_calls = [{ name = "write_file", arguments = { path = "src/lib.rs", content = "x" } }]
            "#,
        )
        .unwrap();
        let arguments: serde_json::Value =
            serde_json::from_str(&responses[0].tool_calls[0].arguments).unwrap();
        assert_eq!(
            arguments,
            serde_json::json!({ "path": "src/lib.rs", "content": "x" })
        );
        assert_eq!(responses[0].tool_calls[0].name, "write_file");
        assert_eq!(responses[0].content, None);
    }

    #[test]
    fn a_string_of_arguments_is_sent_unchanged_even_when_it_is_not_json() {
        let responses = parse(
            "[[responses]]\ntool_calls = [{ name = \"read_file\", arguments = '{\"path\": ' }]\n",
        )
        .unwrap();
        assert_eq!(responses[0].tool_calls[0].arguments, r#"{"path": "#);
    }

    #[test]
    fn a_tool_call_without_arguments_has_an_empty_object() {
        let responses = parse("[[responses]]\ntool_calls = [{ name = \"list_files\" }]\n").unwrap();
        assert_eq!(responses[0].tool_calls[0].arguments, "{}");
    }

    #[test]
    fn tool_calls_without_an_id_are_numbered_across_the_file_and_a_given_id_is_kept() {
        let responses = parse(
            r#"
            [[responses]]
            content = "both"
            tool_calls = [{ name = "a" }, { name = "b", id = "mine" }]
            [[responses]]
            tool_calls = [{ name = "c" }]
            "#,
        )
        .unwrap();
        assert_eq!(
            responses[0].tool_calls,
            [call("call_1", "a", "{}"), call("mine", "b", "{}")]
        );
        assert_eq!(responses[0].content.as_deref(), Some("both"));
        assert_eq!(responses[1].tool_calls, [call("call_3", "c", "{}")]);
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(parse("[[responses]]\ntext = \"hello\"\n").is_err());
        assert!(parse("steps = []\n").is_err());
    }

    #[test]
    fn load_queues_the_responses_in_order_and_then_runs_out() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("fake.toml");
        fs::write(
            &path,
            "[[responses]]\ncontent = \"first\"\n[[responses]]\ncontent = \"second\"\n",
        )
        .unwrap();
        let client = load(&path).unwrap();
        assert_eq!(
            client.chat(&request()).unwrap().content.as_deref(),
            Some("first")
        );
        assert_eq!(
            client.chat(&request()).unwrap().content.as_deref(),
            Some("second")
        );
        assert!(matches!(
            client.chat(&request()),
            Err(ChatError::FakeExhausted)
        ));
    }

    #[test]
    fn load_reports_a_missing_file_with_its_path() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("missing.toml");
        let error = load(&path).unwrap_err();
        assert!(matches!(error, FakeScriptError::Io { .. }));
        assert!(
            error.to_string().contains(&path.display().to_string()),
            "{error}"
        );
    }

    #[test]
    fn load_reports_a_malformed_file_with_its_path() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("fake.toml");
        fs::write(&path, "responses = 1\n").unwrap();
        let error = load(&path).unwrap_err();
        assert!(matches!(error, FakeScriptError::Parse { .. }));
        assert!(error.to_string().contains("invalid fake script"), "{error}");
    }
}
