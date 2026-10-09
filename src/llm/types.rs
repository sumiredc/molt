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
}

impl fmt::Display for ChatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FakeExhausted => write!(f, "the fake chat client has no more queued responses"),
        }
    }
}

impl std::error::Error for ChatError {}

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
