//! OpenAI 互換の chat API（`/chat/completions`・`/models`）を呼ぶ [`ChatClient`]（PRD §5・§8.1）。
//!
//! wire 形式（JSON）への変換はここだけが持つ。形は `tests/fixtures/llm/*.json` で固めてある。

use std::fmt;
use std::net::IpAddr;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::ChatClient;
use super::types::{
    ChatError, ChatRequest, ChatResponse, Message, ResponseFormat, ToolCall, ToolChoice,
};
use crate::config::Api;

/// `[api]` のエンドポイントを reqwest の blocking で呼ぶ [`ChatClient`]。
#[derive(Debug)]
pub struct OpenAiClient {
    http: reqwest::blocking::Client,
    /// 末尾の `/` を除いた `[api]` の `base_url`。
    base_url: String,
    /// `None` なら Authorization ヘッダを送らない。
    api_key: Option<ApiKey>,
}

/// API キー。`Debug` に値を出さない（PRD §3）。
struct ApiKey(String);

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey(<redacted>)")
    }
}

impl OpenAiClient {
    /// `api` の接続先とタイムアウトでクライアントを作る。
    ///
    /// API キーは `api.api_key_env` の名前の環境変数から `var` で読む。名前が空なら
    /// キーを要らないエンドポイント（ローカルのモデルなど）とみなし、Authorization ヘッダを送らない。
    /// 名前があるのに値が無い（未設定・空）ときは [`ChatError::MissingApiKey`]。
    /// キーがあり、`base_url` が TLS 無しの `http://` で loopback 以外のホストを指すときは
    /// [`ChatError::ApiKeyOverPlainHttp`]（キーを平文で機械の外に出さない。PRD §3）。
    /// 呼び出し側は `|name| std::env::var(name).ok()` を渡す。
    pub fn new(api: &Api, var: impl Fn(&str) -> Option<String>) -> Result<Self, ChatError> {
        let api_key = if api.api_key_env.is_empty() {
            None
        } else {
            let key = var(&api.api_key_env)
                .filter(|key| !key.is_empty())
                .ok_or_else(|| ChatError::MissingApiKey {
                    env: api.api_key_env.clone(),
                })?;
            Some(ApiKey(key))
        };
        if api_key.is_some()
            && let Some(host) = plain_http_host_off_this_machine(&api.base_url)
        {
            return Err(ChatError::ApiKeyOverPlainHttp {
                env: api.api_key_env.clone(),
                host,
            });
        }

        // reqwest は rustls の暗号実装を同梱しない features で入れている（ビルド時間とディスクのため、
        // aws-lc-rs の代わりに ring を使う）。既に入っていれば Err が返るだけなので無視する。
        let _ = rustls::crypto::ring::default_provider().install_default();
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(api.timeout_secs))
            .build()
            .map_err(ChatError::Request)?;

        Ok(Self {
            http,
            base_url: api.base_url.trim_end_matches('/').to_owned(),
            api_key,
        })
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}/{path}", self.base_url)
    }

    fn authorize(
        &self,
        builder: reqwest::blocking::RequestBuilder,
    ) -> reqwest::blocking::RequestBuilder {
        match &self.api_key {
            Some(ApiKey(key)) => builder.bearer_auth(key),
            None => builder,
        }
    }

    /// 応答の本文を、API キーを伏せて読む。2xx 以外なら [`ChatError::Status`] にする。
    ///
    /// 2xx の本文も伏せる。形が合わないと serde_json のエラーが本文の値を含み、
    /// [`ChatError::InvalidResponse`] に入るため。
    fn read_body(&self, response: reqwest::blocking::Response) -> Result<String, ChatError> {
        let status = response.status();
        let body = self.redact(response.text().map_err(ChatError::Request)?);
        if !status.is_success() {
            return Err(ChatError::Status {
                status: status.as_u16(),
                body,
            });
        }
        Ok(body)
    }

    /// エンドポイントが本文にキーをそのまま書き返しても、エラーメッセージに出さない。
    fn redact(&self, text: String) -> String {
        match &self.api_key {
            Some(ApiKey(key)) => text.replace(key.as_str(), "<redacted>"),
            None => text,
        }
    }
}

/// `base_url` が TLS 無しの `http://` で loopback 以外のホストを指すなら、その `host:port` を返す。
///
/// URL として読めないときは `None`（検査せず、送るときの [`ChatError::Request`] に任せる）。
fn plain_http_host_off_this_machine(base_url: &str) -> Option<String> {
    let url = reqwest::Url::parse(base_url).ok()?;
    if url.scheme() != "http" {
        return None;
    }
    // IPv6 は `[::1]` の形で返る。IP アドレスとして読めなければホスト名
    let host = url.host_str()?;
    let loopback = match host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
    {
        Ok(ip) => ip.is_loopback(),
        Err(_) => host.eq_ignore_ascii_case("localhost"),
    };
    if loopback {
        return None;
    }
    Some(match url.port_or_known_default() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    })
}

impl ChatClient for OpenAiClient {
    fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, ChatError> {
        let builder = self
            .http
            .post(self.endpoint("chat/completions"))
            .json(&request_body(request));
        let response = self.authorize(builder).send().map_err(ChatError::Request)?;
        parse_chat_response(&self.read_body(response)?)
    }

    fn models(&self) -> Result<Vec<String>, ChatError> {
        let builder = self.http.get(self.endpoint("models"));
        let response = self.authorize(builder).send().map_err(ChatError::Request)?;
        parse_models_response(&self.read_body(response)?)
    }
}

/// `/chat/completions` に送る本文。`None` と空の項目は送らない。ヘッダ（API キー）は含まない。
pub(crate) fn request_body(request: &ChatRequest) -> Value {
    let mut body = Map::new();
    body.insert("model".to_owned(), json!(request.model));
    body.insert(
        "messages".to_owned(),
        request.messages.iter().map(message_json).collect(),
    );
    if !request.tools.is_empty() {
        let tools = request
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "function": {
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.parameters,
                    },
                })
            })
            .collect();
        body.insert("tools".to_owned(), tools);
    }
    if let Some(choice) = request.tool_choice {
        let choice = match choice {
            ToolChoice::Auto => "auto",
            ToolChoice::Required => "required",
        };
        body.insert("tool_choice".to_owned(), json!(choice));
    }
    if let Some(format) = &request.response_format {
        let format = match format {
            ResponseFormat::JsonObject => json!({"type": "json_object"}),
            ResponseFormat::JsonSchema { name, schema } => json!({
                "type": "json_schema",
                "json_schema": {"name": name, "schema": schema},
            }),
        };
        body.insert("response_format".to_owned(), format);
    }
    if let Some(effort) = &request.reasoning_effort {
        body.insert("reasoning_effort".to_owned(), json!(effort));
    }
    Value::Object(body)
}

fn message_json(message: &Message) -> Value {
    match message {
        Message::System { content } => json!({"role": "system", "content": content}),
        Message::User { content } => json!({"role": "user", "content": content}),
        Message::Assistant {
            content,
            tool_calls,
        } => {
            let mut message = json!({"role": "assistant", "content": content});
            if !tool_calls.is_empty() {
                message["tool_calls"] = tool_calls
                    .iter()
                    .map(|call| {
                        json!({
                            "id": call.id,
                            "type": "function",
                            "function": {"name": call.name, "arguments": call.arguments},
                        })
                    })
                    .collect();
            }
            message
        }
        Message::Tool {
            tool_call_id,
            content,
        } => json!({"role": "tool", "tool_call_id": tool_call_id, "content": content}),
    }
}

#[derive(Deserialize)]
struct WireChatResponse {
    choices: Vec<WireChoice>,
}

#[derive(Deserialize)]
struct WireChoice {
    message: WireMessage,
}

#[derive(Deserialize)]
struct WireMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<WireToolCall>>,
}

#[derive(Deserialize)]
struct WireToolCall {
    id: String,
    function: WireFunction,
}

#[derive(Deserialize)]
struct WireFunction {
    name: String,
    /// 仕様では JSON 文字列。オブジェクトで返すエンドポイントもあるので、どちらも受ける。
    arguments: Value,
}

/// `/chat/completions` の応答から、最初の choice の本文と tool_calls を取り出す。
///
/// 引数は parse せずに文字列で持つ（壊れた JSON でも tool_call の `id` を失わないため）。
fn parse_chat_response(body: &str) -> Result<ChatResponse, ChatError> {
    let response: WireChatResponse =
        serde_json::from_str(body).map_err(|e| ChatError::InvalidResponse(e.to_string()))?;
    let choice = response
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| ChatError::InvalidResponse("the response has no choices".to_owned()))?;
    let tool_calls = choice
        .message
        .tool_calls
        .unwrap_or_default()
        .into_iter()
        .map(|call| ToolCall {
            id: call.id,
            name: call.function.name,
            arguments: match call.function.arguments {
                Value::String(arguments) => arguments,
                other => other.to_string(),
            },
        })
        .collect();
    Ok(ChatResponse {
        content: choice.message.content,
        tool_calls,
    })
}

#[derive(Deserialize)]
struct WireModels {
    data: Vec<WireModel>,
}

#[derive(Deserialize)]
struct WireModel {
    id: String,
}

/// `/models` の応答からモデルの ID を取り出す。
fn parse_models_response(body: &str) -> Result<Vec<String>, ChatError> {
    let models: WireModels =
        serde_json::from_str(body).map_err(|e| ChatError::InvalidResponse(e.to_string()))?;
    Ok(models.data.into_iter().map(|model| model.id).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::types::{ToolDefinition, parse_arguments};

    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::thread::{self, JoinHandle};

    const KEY: &str = "sk-test-0123456789abcdef";

    fn fixture(name: &str) -> Value {
        let path = format!("{}/tests/fixtures/llm/{name}", env!("CARGO_MANIFEST_DIR"));
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path}: {e}"))
    }

    fn fixture_text(name: &str) -> String {
        fixture(name).to_string()
    }

    fn request(model: &str, messages: Vec<Message>) -> ChatRequest {
        ChatRequest {
            model: model.to_owned(),
            messages,
            tools: Vec::new(),
            tool_choice: None,
            response_format: None,
            reasoning_effort: None,
        }
    }

    fn user(text: &str) -> Message {
        Message::User {
            content: text.to_owned(),
        }
    }

    fn judgement_schema() -> Value {
        json!({
            "type": "object",
            "properties": {"correct": {"type": "boolean"}},
            "required": ["correct"],
        })
    }

    fn api(base_url: &str, api_key_env: &str, timeout_secs: u64) -> Api {
        Api {
            base_url: base_url.to_owned(),
            api_key_env: api_key_env.to_owned(),
            timeout_secs,
        }
    }

    fn with_key(name: &str) -> Option<String> {
        (name == "TEST_API_KEY").then(|| KEY.to_owned())
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    /// テストの中で立てる 1 回だけ応答する HTTP サーバが受け取ったもの。
    struct Received {
        request_line: String,
        /// ヘッダ名は小文字にしてある。
        headers: Vec<(String, String)>,
        body: String,
    }

    impl Received {
        fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
        }
    }

    /// 127.0.0.1 で 1 回だけリクエストを受け、`status` と `body` を返すサーバを立てる。
    /// 戻り値は base URL（`http://127.0.0.1:<port>/v1`）と、受け取ったものを返す thread。
    fn serve_once(status: u16, body: &str) -> (String, JoinHandle<Received>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
        let body = body.to_owned();
        let handle = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();
            let mut headers = Vec::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let line = line.trim_end();
                if line.is_empty() {
                    break;
                }
                let (key, value) = line.split_once(':').unwrap();
                headers.push((key.trim().to_lowercase(), value.trim().to_owned()));
            }
            let length = headers
                .iter()
                .find(|(key, _)| key == "content-length")
                .map_or(0, |(_, value)| value.parse().unwrap());
            let mut request_body = vec![0; length];
            reader.read_exact(&mut request_body).unwrap();

            let mut stream = stream;
            write!(
                stream,
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            Received {
                request_line: request_line.trim_end().to_owned(),
                headers,
                body: String::from_utf8(request_body).unwrap(),
            }
        });
        (base_url, handle)
    }

    // --- 組み立てるリクエストと fixture の一致（#18 の条件 2） ---

    #[test]
    fn plain_chat_request_matches_the_fixture() {
        let request = request(
            "nvidia/Nemotron-3_5-Lightning",
            vec![
                Message::System {
                    content: "You are molt.".to_owned(),
                },
                user("Say hello."),
            ],
        );
        assert_eq!(request_body(&request), fixture("chat.json"));
    }

    #[test]
    fn request_with_tools_and_a_tool_round_trip_matches_the_fixture() {
        let mut request = request(
            "nvidia/nemotron-3-super-120b-a12b",
            vec![
                user("Read src/main.rs."),
                Message::Assistant {
                    content: None,
                    tool_calls: vec![ToolCall {
                        id: "call_0".to_owned(),
                        name: "read_file".to_owned(),
                        arguments: r#"{"path":"src/main.rs"}"#.to_owned(),
                    }],
                },
                Message::Tool {
                    tool_call_id: "call_0".to_owned(),
                    content: "fn main() {}".to_owned(),
                },
            ],
        );
        request.tools = vec![ToolDefinition {
            name: "read_file".to_owned(),
            description: "Read a file under the project root.".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"],
            }),
        }];
        request.tool_choice = Some(ToolChoice::Required);
        assert_eq!(request_body(&request), fixture("tools.json"));
    }

    #[test]
    fn request_with_json_schema_response_format_matches_the_fixture() {
        let mut request = request(
            "nvidia/Nemotron-3-Ultra-550b-a55b",
            vec![user("Judge the answer.")],
        );
        request.response_format = Some(ResponseFormat::JsonSchema {
            name: "judgement".to_owned(),
            schema: judgement_schema(),
        });
        assert_eq!(
            request_body(&request),
            fixture("response_format_json_schema.json")
        );
    }

    #[test]
    fn request_with_json_object_response_format_matches_the_fixture() {
        let mut request = request(
            "nvidia/Nemotron-3-Ultra-550b-a55b",
            vec![user("Judge the answer.")],
        );
        request.response_format = Some(ResponseFormat::JsonObject);
        assert_eq!(
            request_body(&request),
            fixture("response_format_json_object.json")
        );
    }

    #[test]
    fn request_with_reasoning_effort_matches_the_fixture() {
        let mut request = request(
            "nvidia/Nemotron-3-Ultra-550b-a55b",
            vec![user("Explain the boundary.")],
        );
        request.reasoning_effort = Some("high".to_owned());
        assert_eq!(request_body(&request), fixture("reasoning_effort.json"));
    }

    #[test]
    fn request_without_reasoning_effort_has_no_reasoning_effort_key() {
        let request = request(
            "nvidia/Nemotron-3-Ultra-550b-a55b",
            vec![user("Explain the boundary.")],
        );
        assert_eq!(request_body(&request).get("reasoning_effort"), None);
    }

    #[test]
    fn chat_posts_the_request_body_to_chat_completions() {
        let (base_url, server) = serve_once(200, &fixture_text("response_text.json"));
        let client = OpenAiClient::new(&api(&base_url, "", 5), no_env).unwrap();
        let request = request(
            "nvidia/Nemotron-3_5-Lightning",
            vec![
                Message::System {
                    content: "You are molt.".to_owned(),
                },
                user("Say hello."),
            ],
        );

        client.chat(&request).unwrap();

        let received = server.join().unwrap();
        assert_eq!(received.request_line, "POST /v1/chat/completions HTTP/1.1");
        let sent: Value = serde_json::from_str(&received.body).unwrap();
        assert_eq!(sent, fixture("chat.json"));
    }

    #[test]
    fn trailing_slash_in_base_url_does_not_double_the_slash() {
        let (base_url, server) = serve_once(200, &fixture_text("models.json"));
        let client = OpenAiClient::new(&api(&format!("{base_url}/"), "", 5), no_env).unwrap();

        client.models().unwrap();

        assert_eq!(
            server.join().unwrap().request_line,
            "GET /v1/models HTTP/1.1"
        );
    }

    // --- 認証 ---

    #[test]
    fn api_key_from_the_named_environment_variable_is_sent_as_bearer() {
        let (base_url, server) = serve_once(200, &fixture_text("response_text.json"));
        let client = OpenAiClient::new(&api(&base_url, "TEST_API_KEY", 5), with_key).unwrap();

        client.chat(&request("m", vec![user("hi")])).unwrap();

        let received = server.join().unwrap();
        assert_eq!(
            received.header("authorization"),
            Some(format!("Bearer {KEY}").as_str())
        );
    }

    #[test]
    fn empty_api_key_env_sends_no_authorization_header() {
        let (base_url, server) = serve_once(200, &fixture_text("response_text.json"));
        let client = OpenAiClient::new(&api(&base_url, "", 5), with_key).unwrap();

        client.chat(&request("m", vec![user("hi")])).unwrap();

        assert_eq!(server.join().unwrap().header("authorization"), None);
    }

    #[test]
    fn named_api_key_env_that_is_unset_or_empty_is_missing_api_key() {
        let empty = |_: &str| Some(String::new());
        for var in [&no_env as &dyn Fn(&str) -> Option<String>, &empty] {
            let error = OpenAiClient::new(&api("http://127.0.0.1:1/v1", "TEST_API_KEY", 5), var)
                .unwrap_err();
            assert!(
                matches!(&error, ChatError::MissingApiKey { env } if env == "TEST_API_KEY"),
                "{error:?}"
            );
        }
    }

    // --- 平文の HTTP で機械の外へキーを送らない（PRD §3） ---

    #[test]
    fn api_key_over_plain_http_to_a_non_loopback_host_is_refused() {
        let error = OpenAiClient::new(
            &api("http://192.0.2.1:8000/v1", "TEST_API_KEY", 5),
            with_key,
        )
        .unwrap_err();
        assert!(
            matches!(
                &error,
                ChatError::ApiKeyOverPlainHttp { env, host }
                    if env == "TEST_API_KEY" && host == "192.0.2.1:8000"
            ),
            "{error:?}"
        );
        let display = error.to_string();
        let debug = format!("{error:?}");
        assert!(display.contains("https"), "{display}");
        assert!(display.contains("api_key_env"), "{display}");
        assert!(!display.contains(KEY), "{display}");
        assert!(!debug.contains(KEY), "{debug}");
    }

    #[test]
    fn api_key_over_plain_http_to_a_host_name_is_refused() {
        let error = OpenAiClient::new(&api("http://gpu-box.lan/v1", "TEST_API_KEY", 5), with_key)
            .unwrap_err();
        assert!(
            matches!(&error, ChatError::ApiKeyOverPlainHttp { host, .. } if host == "gpu-box.lan:80"),
            "{error:?}"
        );
    }

    #[test]
    fn userinfo_in_base_url_does_not_appear_in_the_refusal() {
        let error = OpenAiClient::new(
            &api("http://user:secret@192.0.2.1:8000/v1", "TEST_API_KEY", 5),
            with_key,
        )
        .unwrap_err();
        assert!(
            matches!(&error, ChatError::ApiKeyOverPlainHttp { host, .. } if host == "192.0.2.1:8000"),
            "{error:?}"
        );
        let display = error.to_string();
        let debug = format!("{error:?}");
        assert!(!display.contains("secret"), "{display}");
        assert!(!debug.contains("secret"), "{debug}");
    }

    #[test]
    fn api_key_over_plain_http_to_loopback_is_allowed() {
        for base_url in [
            "http://localhost:8000/v1",
            "http://127.0.0.1:8000/v1",
            "http://127.0.0.2:8000/v1",
            "http://[::1]:8000/v1",
        ] {
            let result = OpenAiClient::new(&api(base_url, "TEST_API_KEY", 5), with_key);
            assert!(result.is_ok(), "{base_url}: {result:?}");
        }
    }

    #[test]
    fn plain_http_to_a_non_loopback_host_is_allowed_without_an_api_key() {
        let result = OpenAiClient::new(&api("http://192.0.2.1:8000/v1", "", 5), with_key);
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn unparsable_base_url_is_left_to_the_request_error() {
        let result = OpenAiClient::new(&api("not a url", "TEST_API_KEY", 5), with_key);
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn api_key_over_https_to_a_non_loopback_host_is_allowed() {
        let result = OpenAiClient::new(&api("https://example.com/v1", "TEST_API_KEY", 5), with_key);
        assert!(result.is_ok(), "{result:?}");
    }

    // --- API キーを漏らさない（#18 の条件 6 の一部） ---

    #[test]
    fn debug_output_of_the_client_does_not_contain_the_api_key() {
        let client =
            OpenAiClient::new(&api("http://127.0.0.1:1/v1", "TEST_API_KEY", 5), with_key).unwrap();
        let debug = format!("{client:?}");
        assert!(!debug.contains(KEY), "{debug}");
    }

    #[test]
    fn error_status_echoing_the_api_key_does_not_leak_it_into_the_error() {
        let echoed = format!(r#"{{"error": {{"message": "Incorrect API key provided: {KEY}"}}}}"#);
        let (base_url, server) = serve_once(401, &echoed);
        let client = OpenAiClient::new(&api(&base_url, "TEST_API_KEY", 5), with_key).unwrap();

        let error = client.chat(&request("m", vec![user("hi")])).unwrap_err();
        server.join().unwrap();

        assert!(
            matches!(&error, ChatError::Status { status: 401, body } if body.contains("Incorrect API key")),
            "{error:?}"
        );
        let display = error.to_string();
        let debug = format!("{error:?}");
        assert!(!display.contains(KEY), "{display}");
        assert!(!debug.contains(KEY), "{debug}");
    }

    #[test]
    fn unreadable_success_body_echoing_the_api_key_does_not_leak_it_into_the_error() {
        let echoed = format!(r#"{{"choices": "Incorrect API key provided: {KEY}"}}"#);
        let (base_url, server) = serve_once(200, &echoed);
        let client = OpenAiClient::new(&api(&base_url, "TEST_API_KEY", 5), with_key).unwrap();

        let error = client.chat(&request("m", vec![user("hi")])).unwrap_err();
        server.join().unwrap();

        assert!(matches!(error, ChatError::InvalidResponse(_)), "{error:?}");
        let display = error.to_string();
        let debug = format!("{error:?}");
        assert!(!display.contains(KEY), "{display}");
        assert!(!debug.contains(KEY), "{debug}");
    }

    #[test]
    fn transport_errors_do_not_contain_the_api_key() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
        drop(listener);
        let client = OpenAiClient::new(&api(&base_url, "TEST_API_KEY", 5), with_key).unwrap();

        let error = client.chat(&request("m", vec![user("hi")])).unwrap_err();

        assert!(matches!(error, ChatError::Request(_)), "{error:?}");
        let display = error.to_string();
        let debug = format!("{error:?}");
        assert!(!display.contains(KEY), "{display}");
        assert!(!debug.contains(KEY), "{debug}");
    }

    // --- タイムアウト ---

    #[test]
    fn a_response_slower_than_timeout_secs_is_a_request_error() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
        // 接続は受けるが応答しない
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            thread::sleep(Duration::from_secs(3));
            drop(stream);
        });
        let client = OpenAiClient::new(&api(&base_url, "", 1), no_env).unwrap();

        let error = client.chat(&request("m", vec![user("hi")])).unwrap_err();

        assert!(
            matches!(&error, ChatError::Request(source) if source.is_timeout()),
            "{error:?}"
        );
        server.join().unwrap();
    }

    // --- 応答の読み取り ---

    #[test]
    fn text_response_is_read_into_content() {
        let response = parse_chat_response(&fixture_text("response_text.json")).unwrap();
        assert_eq!(
            response,
            ChatResponse {
                content: Some("Hello.".to_owned()),
                tool_calls: Vec::new(),
            }
        );
    }

    #[test]
    fn tool_call_with_broken_arguments_keeps_its_id_and_raw_arguments() {
        let response = parse_chat_response(&fixture_text("response_tool_calls.json")).unwrap();
        assert_eq!(
            response,
            ChatResponse {
                content: None,
                tool_calls: vec![ToolCall {
                    id: "call_0".to_owned(),
                    name: "read_file".to_owned(),
                    arguments: r#"{"path":"src/main.rs",}"#.to_owned(),
                }],
            }
        );
        // 寛容な parse（#18 の条件 3）は、実クライアントの応答にもそのまま当てられる
        assert_eq!(
            parse_arguments(&response.tool_calls[0]).unwrap(),
            json!({"path": "src/main.rs"}).as_object().unwrap().clone()
        );
    }

    #[test]
    fn tool_call_arguments_returned_as_an_object_are_kept_as_a_json_string() {
        let body = json!({
            "choices": [{"message": {"tool_calls": [{
                "id": "call_1",
                "type": "function",
                "function": {"name": "read_file", "arguments": {"path": "a.rs"}},
            }]}}],
        });
        let response = parse_chat_response(&body.to_string()).unwrap();
        assert_eq!(response.tool_calls[0].id, "call_1");
        assert_eq!(
            serde_json::from_str::<Value>(&response.tool_calls[0].arguments).unwrap(),
            json!({"path": "a.rs"})
        );
    }

    #[test]
    fn response_without_choices_is_invalid() {
        let error = parse_chat_response(r#"{"choices": []}"#).unwrap_err();
        assert!(matches!(error, ChatError::InvalidResponse(_)), "{error:?}");
    }

    #[test]
    fn models_returns_the_ids_from_the_models_endpoint() {
        let (base_url, server) = serve_once(200, &fixture_text("models.json"));
        let client = OpenAiClient::new(&api(&base_url, "", 5), no_env).unwrap();

        let models = client.models().unwrap();

        assert_eq!(
            models,
            [
                "nvidia/Nemotron-3_5-Lightning",
                "nvidia/nemotron-3-super-120b-a12b"
            ]
        );
        assert_eq!(
            server.join().unwrap().request_line,
            "GET /v1/models HTTP/1.1"
        );
    }
}
