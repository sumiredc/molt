//! 実 API に対する `llm` のテスト（PRD §9）。すべて `#[ignore]` で、CI では走らない。
//!
//! `MOLT_TEST_CONFIG=<config.toml のパス>` の `[api]` と `[models]` の light のモデルに対して走らせる。
//! 未設定なら何もせずに通る（skip）。
//!
//! ```sh
//! MOLT_TEST_CONFIG=~/.config/molt/config.toml cargo test --locked llm -- --ignored --nocapture
//! ```

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use serde::Deserialize;
use serde_json::{Value, json};

use molt::config::{Capabilities, Config};
use molt::llm::types::{check_tool_name, parse_arguments};
use molt::llm::{
    ChatClient, ChatError, ChatRequest, ChatResponse, Message, OpenAiClient, ResponseFormat, Role,
    Router, ToolChoice, ToolDefinition,
};

/// テストを 1 つずつ走らせるための錠。ローカルのエンドポイントは同時に受けられるリクエストが少なく、
/// Nebius ではクレジットを使うので、cargo test のスレッド並列でも同時に 1 リクエストにする。
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// `MOLT_TEST_CONFIG` の設定と、その `[api]` に向けたクライアント。未設定なら `None`。
struct RealApi {
    config: Config,
    client: OpenAiClient,
    _lock: MutexGuard<'static, ()>,
}

impl RealApi {
    fn model(&self) -> String {
        self.config.models.light.clone()
    }
}

fn real_api() -> Option<RealApi> {
    let Some(path) = std::env::var_os("MOLT_TEST_CONFIG").filter(|path| !path.is_empty()) else {
        eprintln!("skipped: MOLT_TEST_CONFIG is not set");
        return None;
    };
    let path = PathBuf::from(path);
    // Config::load は無いファイルを既定値（Nebius）として読むので、パスの誤りで既定の接続先に送らないよう先に止める
    assert!(
        path.is_file(),
        "MOLT_TEST_CONFIG does not point to a file: {}",
        path.display()
    );
    let config = Config::load(&path).unwrap_or_else(|e| panic!("{e}"));
    let client = OpenAiClient::new(&config.api, |name| std::env::var(name).ok())
        .unwrap_or_else(|e| panic!("{e}"));
    // 先に panic したテストが錠を毒しても、後のテストは走らせる
    let lock = ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    Some(RealApi {
        config,
        client,
        _lock: lock,
    })
}

fn request(model: &str, prompt: &str) -> ChatRequest {
    ChatRequest {
        model: model.to_owned(),
        messages: vec![Message::User {
            content: prompt.to_owned(),
        }],
        tools: Vec::new(),
        tool_choice: None,
        response_format: None,
        reasoning_effort: None,
        stream: false,
    }
}

fn answer_schema() -> Value {
    json!({
        "type": "object",
        "properties": {"answer": {"type": "integer"}},
        "required": ["answer"],
        "additionalProperties": false,
    })
}

/// [`answer_schema`] に従う JSON。`additionalProperties: false` に合わせて余分なキーを拒否する。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Answer {
    #[allow(dead_code)]
    answer: i64,
}

/// 送ったリクエストを記録しながら、実クライアントにそのまま渡す。
struct Recording<'a> {
    inner: &'a dyn ChatClient,
    requests: Mutex<Vec<ChatRequest>>,
}

impl ChatClient for Recording<'_> {
    fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, ChatError> {
        self.requests.lock().unwrap().push(request.clone());
        self.inner.chat(request)
    }

    fn models(&self) -> Result<Vec<String>, ChatError> {
        self.inner.models()
    }
}

mod llm {
    use super::*;

    #[test]
    #[ignore = "calls the real API; set MOLT_TEST_CONFIG"]
    fn chat_returns_a_non_empty_reply() {
        let Some(api) = real_api() else { return };

        let response = api
            .client
            .chat(&request(&api.model(), "Reply with the single word OK."))
            .unwrap();

        eprintln!("chat: {response:?}");
        let content = response.content.unwrap_or_default();
        assert!(!content.trim().is_empty(), "the reply had no content");
    }

    #[test]
    #[ignore = "calls the real API; set MOLT_TEST_CONFIG"]
    fn tool_choice_required_returns_a_call_to_the_given_tool_with_object_arguments() {
        let Some(api) = real_api() else { return };
        let tools = vec![ToolDefinition {
            name: "record_answer".to_owned(),
            description: "Record the answer to the question.".to_owned(),
            parameters: answer_schema(),
        }];
        let mut request = request(
            &api.model(),
            "What is 2 + 3? Record the answer with the tool.",
        );
        request.tools = tools.clone();
        request.tool_choice = Some(ToolChoice::Required);

        let response = api.client.chat(&request).unwrap();

        eprintln!("tool_choice: required: {response:?}");
        assert!(
            !response.tool_calls.is_empty(),
            "the reply had no tool call"
        );
        for call in &response.tool_calls {
            check_tool_name(call, &tools).unwrap();
            let arguments = parse_arguments(call).unwrap();
            serde_json::from_value::<Answer>(Value::Object(arguments)).unwrap();
        }
    }

    /// 引数の無いツールに `arguments: ""` を返すエンドポイントがあると、`parse_arguments` が
    /// `InvalidArguments` にし、本線（#20）の再送を無駄に使う。そうならないかを確かめる。
    #[test]
    #[ignore = "calls the real API; set MOLT_TEST_CONFIG"]
    fn tool_call_without_parameters_has_arguments_that_parse_as_an_object() {
        let Some(api) = real_api() else { return };
        let tools = vec![ToolDefinition {
            name: "finish".to_owned(),
            description: "Signal that the task is finished. Takes no arguments.".to_owned(),
            parameters: json!({"type": "object", "properties": {}}),
        }];
        let mut request = request(&api.model(), "The task is finished. Call the tool.");
        request.tools = tools.clone();
        request.tool_choice = Some(ToolChoice::Required);

        let response = api.client.chat(&request).unwrap();

        eprintln!("tool without parameters: {response:?}");
        assert!(
            !response.tool_calls.is_empty(),
            "the reply had no tool call"
        );
        for call in &response.tool_calls {
            check_tool_name(call, &tools).unwrap();
            parse_arguments(call).unwrap();
        }
    }

    #[test]
    #[ignore = "calls the real API; set MOLT_TEST_CONFIG"]
    fn json_schema_reply_follows_the_schema() {
        let Some(api) = real_api() else { return };
        let mut request = request(&api.model(), "What is 2 + 3? Reply in JSON.");
        request.response_format = Some(ResponseFormat::JsonSchema {
            name: "answer".to_owned(),
            schema: answer_schema(),
        });

        let response = api.client.chat(&request).unwrap();

        eprintln!("json_schema: {response:?}");
        let content = response.content.expect("the reply had no content");
        serde_json::from_str::<Answer>(&content).unwrap();
    }

    /// `[capabilities]` の `json_schema` が偽のとき、`Router::chat_json` が `json_object` +
    /// プロンプトのスキーマ（+ 1 回の修復）に切り替えて、スキーマどおりの値を返す（PRD §8.7）。
    /// `doctor` の表はこの経路を確かめないので、ここで実際のエンドポイントに当てる。
    #[test]
    #[ignore = "calls the real API; set MOLT_TEST_CONFIG"]
    fn chat_json_without_json_schema_switches_to_json_object_and_returns_the_schema() {
        let Some(api) = real_api() else { return };
        let mut config = api.config.clone();
        // json_schema も reasoning_effort も使えないものとして扱わせる
        config.capabilities = Capabilities::default();
        let router = Router::new(&config);
        let client = Recording {
            inner: &api.client,
            requests: Mutex::new(Vec::new()),
        };

        let answer: Answer = router
            .chat_json(
                &client,
                Role::Light,
                vec![Message::User {
                    content: "What is 2 + 3?".to_owned(),
                }],
                "answer",
                &answer_schema(),
            )
            .unwrap();

        let requests = client.requests.into_inner().unwrap();
        eprintln!(
            "chat_json (json_object): {answer:?} after {} request(s)",
            requests.len()
        );
        assert_eq!(
            requests[0].response_format,
            Some(ResponseFormat::JsonObject)
        );
    }
}
