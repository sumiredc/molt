//! chat API のリクエスト・レスポンス・ツール定義の型と、応答の tool_calls の名前の検査。
//!
//! wire 形式（JSON）への変換は実クライアントが持つ。ここの型は agent と `molt doctor` が組み立てて読む形。

use std::fmt;

use serde_json::Value;

/// 1 回の chat の呼び出しで送るもの。
#[derive(Debug, Clone, PartialEq)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    /// モデルが呼び出してよいツール。空ならツールを渡さない。
    pub tools: Vec<ToolDefinition>,
    /// `None` ならエンドポイントの既定に任せる。
    pub tool_choice: Option<ToolChoice>,
    /// `None` なら自由な文章で返させる。
    pub response_format: Option<ResponseFormat>,
    /// `None` なら送らない。値は `[reasoning_effort]` の文字列をそのまま使う。
    pub reasoning_effort: Option<String>,
}

/// 会話の 1 件。
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    System {
        content: String,
    },
    User {
        content: String,
    },
    Assistant {
        content: Option<String>,
        tool_calls: Vec<ToolCall>,
    },
    /// ツールの実行結果。`tool_call_id` は対応する [`ToolCall::id`]。
    Tool {
        tool_call_id: String,
        content: String,
    },
}

/// モデルに渡す function calling のツール定義。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    /// 引数の JSON Schema。
    pub parameters: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolChoice {
    /// ツールを呼ぶかはモデルが決める。
    Auto,
    /// 必ずどれかのツールを呼ばせる。
    Required,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ResponseFormat {
    /// JSON で返させる。スキーマはプロンプトで伝える。
    JsonObject,
    /// `schema` に従う JSON で返させる。
    JsonSchema { name: String, schema: Value },
}

/// モデルの応答。
#[derive(Debug, Clone, PartialEq)]
pub struct ChatResponse {
    pub content: Option<String>,
    pub tool_calls: Vec<ToolCall>,
}

/// モデルが求めたツールの呼び出し 1 件。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// モデルが返した引数の JSON 文字列。壊れていることがあるので、parse せずに持つ。
    pub arguments: String,
}

#[derive(Debug)]
pub enum ChatError {
    /// [`FakeChatClient`](super::FakeChatClient) に積んだ応答を使い切った。
    FakeExhausted,
    /// `[api]` の `api_key_env` に名前があるのに、その環境変数が未設定か空。値ではなく名前だけを持つ。
    MissingApiKey { env: String },
    /// エンドポイントに届かない、タイムアウトした、応答を受け取り切れなかった。
    /// クライアントを作れなかった（TLS の初期化など）ときも、まれにこれになる。
    /// `molt doctor` が機能の欠落と分けて「接続の失敗」として報告するのに使う。
    Request(reqwest::Error),
    /// エンドポイントが 2xx 以外を返した。`body` は応答の本文で、API キーは伏せてある。
    Status { status: u16, body: String },
    /// 応答の本文が chat API の形として読めない。
    InvalidResponse(String),
}

impl fmt::Display for ChatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FakeExhausted => write!(f, "the fake chat client has no more queued responses"),
            Self::MissingApiKey { env } => {
                write!(f, "the API key environment variable `{env}` is not set")
            }
            Self::Request(source) => write!(f, "request to the chat API failed: {source}"),
            Self::Status { status, body } => {
                write!(f, "the chat API returned HTTP {status}: {body}")
            }
            Self::InvalidResponse(reason) => {
                write!(f, "the chat API returned an unexpected response: {reason}")
            }
        }
    }
}

impl std::error::Error for ChatError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Request(source) => Some(source),
            _ => None,
        }
    }
}

/// tool_call の名前が、リクエストで渡したツールに無かった。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownTool {
    pub name: String,
    /// リクエストで渡したツール名。モデルに返して呼び直させるのに使う（PRD §11）。
    pub valid: Vec<String>,
}

impl fmt::Display for UnknownTool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown tool `{}`; valid tools are: {}",
            self.name,
            self.valid.join(", ")
        )
    }
}

impl std::error::Error for UnknownTool {}

/// `call` の名前が `tools` にあるかを確かめる。無ければ有効なツール名の一覧と一緒に
/// [`UnknownTool`] を返す。
///
/// `chat` は応答をそのまま返し、呼び出し側が tool_call ごとにこれで確かめる。エラーを
/// その tool_call の `id` に対するツール結果としてモデルに返せるようにするため（PRD §11）。
/// 実クライアントと fake のどちらの応答にも同じ関数を当てるので、結果が揃う。
pub fn check_tool_name(call: &ToolCall, tools: &[ToolDefinition]) -> Result<(), UnknownTool> {
    if tools.iter().any(|tool| tool.name == call.name) {
        return Ok(());
    }
    Err(UnknownTool {
        name: call.name.clone(),
        valid: tools.iter().map(|tool| tool.name.clone()).collect(),
    })
}

/// tool_call の引数が JSON のオブジェクトとして読めなかった。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidArguments {
    /// 対応する [`ToolCall::id`]。エラーをこの id へのツール結果としてモデルに返し、呼び直させる（PRD §11）。
    pub id: String,
    pub name: String,
    pub reason: String,
}

impl fmt::Display for InvalidArguments {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the arguments of tool `{}` are not a JSON object: {}",
            self.name, self.reason
        )
    }
}

impl std::error::Error for InvalidArguments {}

/// `call` の引数を JSON のオブジェクトとして読む。モデルが崩しがちな 2 つの形は寛容に読む（PRD §11）。
///
/// - 末尾カンマ（`{"a": 1,}`、`[1, 2,]`）は取り除いて読む
/// - 文字列化された JSON（`"{\"a\": 1}"`）は、中の文字列をもう一度読む
///
/// それでもオブジェクトにならなければ [`InvalidArguments`] を返す。モデルへの再送は呼び出し側の loop が行う。
/// [`check_tool_name`] と同じく、実クライアントと fake のどちらの応答にも呼び出し側が当てる。
pub fn parse_arguments(
    call: &ToolCall,
) -> Result<serde_json::Map<String, Value>, InvalidArguments> {
    let invalid = |reason: String| InvalidArguments {
        id: call.id.clone(),
        name: call.name.clone(),
        reason,
    };
    let mut value = parse_lenient(&call.arguments).map_err(|e| invalid(e.to_string()))?;
    if let Value::String(inner) = &value {
        value = parse_lenient(inner).map_err(|e| invalid(e.to_string()))?;
    }
    match value {
        Value::Object(map) => Ok(map),
        other => Err(invalid(format!("expected an object, got `{other}`"))),
    }
}

/// `text` を JSON として読む。読めなければ、文字列の外にある末尾カンマを取り除いてもう一度読む。
fn parse_lenient(text: &str) -> serde_json::Result<Value> {
    serde_json::from_str(text).or_else(|first| {
        let stripped = strip_trailing_commas(text);
        if stripped == text {
            return Err(first);
        }
        serde_json::from_str(&stripped).map_err(|_| first)
    })
}

/// 文字列リテラルの外で、空白を挟んで `}` か `]` の直前にあるカンマを取り除く。
fn strip_trailing_commas(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_string = false;
    let mut escaped = false;
    for (i, c) in text.char_indices() {
        if in_string {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
        } else if c == '"' {
            in_string = true;
        } else if c == ',' {
            let next = text[i + 1..].trim_start().chars().next();
            if matches!(next, Some('}' | ']')) {
                continue;
            }
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool(name: &str) -> ToolDefinition {
        ToolDefinition {
            name: name.to_owned(),
            description: String::new(),
            parameters: json!({"type": "object"}),
        }
    }

    fn call(name: &str) -> ToolCall {
        ToolCall {
            id: "call_0".to_owned(),
            name: name.to_owned(),
            arguments: "{}".to_owned(),
        }
    }

    #[test]
    fn tool_call_with_a_given_name_passes() {
        let tools = [tool("read_file"), tool("write_file")];
        assert!(check_tool_name(&call("write_file"), &tools).is_ok());
    }

    #[test]
    fn unknown_tool_name_is_an_error_carrying_every_valid_name() {
        let tools = [tool("read_file"), tool("write_file")];
        assert_eq!(
            check_tool_name(&call("delete_file"), &tools),
            Err(UnknownTool {
                name: "delete_file".to_owned(),
                valid: vec!["read_file".to_owned(), "write_file".to_owned()],
            })
        );
    }

    #[test]
    fn unknown_tool_error_message_lists_every_valid_name() {
        let tools = [tool("read_file"), tool("write_file")];
        let message = check_tool_name(&call("delete_file"), &tools)
            .unwrap_err()
            .to_string();
        assert!(message.contains("delete_file"), "{message}");
        assert!(message.contains("read_file"), "{message}");
        assert!(message.contains("write_file"), "{message}");
    }

    fn call_with_arguments(arguments: &str) -> ToolCall {
        ToolCall {
            id: "call_3".to_owned(),
            name: "write_file".to_owned(),
            arguments: arguments.to_owned(),
        }
    }

    fn object(value: Value) -> serde_json::Map<String, Value> {
        match value {
            Value::Object(map) => map,
            other => panic!("not an object: {other}"),
        }
    }

    #[test]
    fn parse_arguments_reads_well_formed_json() {
        assert_eq!(
            parse_arguments(&call_with_arguments(r#"{"path": "a.rs", "lines": [1, 2]}"#)),
            Ok(object(json!({"path": "a.rs", "lines": [1, 2]})))
        );
    }

    #[test]
    fn parse_arguments_tolerates_trailing_commas_in_objects_and_arrays() {
        assert_eq!(
            parse_arguments(&call_with_arguments(
                "{\"path\": \"a.rs\", \"lines\": [1, 2, ],\n}"
            )),
            Ok(object(json!({"path": "a.rs", "lines": [1, 2]})))
        );
    }

    #[test]
    fn parse_arguments_keeps_commas_inside_strings() {
        assert_eq!(
            parse_arguments(&call_with_arguments(r#"{"text": "a,}b,]\",}",}"#)),
            Ok(object(json!({"text": "a,}b,]\",}"})))
        );
    }

    #[test]
    fn parse_arguments_reads_stringified_json() {
        assert_eq!(
            parse_arguments(&call_with_arguments(r#""{\"path\": \"a.rs\"}""#)),
            Ok(object(json!({"path": "a.rs"})))
        );
    }

    #[test]
    fn parse_arguments_reads_stringified_json_with_a_trailing_comma() {
        assert_eq!(
            parse_arguments(&call_with_arguments(r#""{\"path\": \"a.rs\",}""#)),
            Ok(object(json!({"path": "a.rs"})))
        );
    }

    #[test]
    fn parse_arguments_rejects_broken_json_keeping_the_tool_call_id() {
        let error = parse_arguments(&call_with_arguments(r#"{"path": "a.rs""#)).unwrap_err();
        assert_eq!(
            (error.id.as_str(), error.name.as_str()),
            ("call_3", "write_file")
        );
    }

    #[test]
    fn parse_arguments_rejects_json_that_is_not_an_object() {
        for arguments in ["[1, 2]", "42", r#""just text""#, r#""[1]""#] {
            let error = parse_arguments(&call_with_arguments(arguments)).unwrap_err();
            assert_eq!(error.id, "call_3", "{arguments}");
        }
    }

    #[test]
    fn tool_call_when_no_tools_were_given_is_unknown() {
        assert_eq!(
            check_tool_name(&call("read_file"), &[]),
            Err(UnknownTool {
                name: "read_file".to_owned(),
                valid: Vec::new(),
            })
        );
    }
}
