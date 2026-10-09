//! 役割（light / generate / reason）から、送るリクエストの形を決める（PRD §8.7・§11）。
//!
//! モデル ID は `[models]`、`reasoning_effort` は `[reasoning_effort]` から取り、`reasoning_effort` は
//! `[capabilities]` でその役割が受け付けると分かっているときだけ送る。JSON で答えさせるときは、
//! `json_schema` が使えれば `response_format: json_schema`、使えなければ `json_object` +
//! プロンプトにスキーマ + serde での検証 + 1 回の修復に切り替える。

use std::fmt;

use serde::de::DeserializeOwned;
use serde_json::Value;

use super::ChatClient;
use super::types::{ChatError, ChatRequest, Message, ResponseFormat};
use crate::config::{Capabilities, Config, Models, ReasoningEffort, RoleCapabilities};

/// モデルの役割（PRD §8.7）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Light,
    Generate,
    Reason,
}

/// 役割ごとのモデル ID・`reasoning_effort`・`[capabilities]` を持ち、リクエストを組み立てる。
#[derive(Debug, Clone)]
pub struct Router {
    models: Models,
    reasoning_effort: ReasoningEffort,
    capabilities: Capabilities,
}

/// [`Router::chat_json`] の失敗。
#[derive(Debug)]
pub enum JsonError {
    /// chat の呼び出しそのものが失敗した。
    Chat(ChatError),
    /// 応答が JSON として読めない、または型に合わない。`json_object` の経路では修復を 1 回試した後。
    Invalid {
        /// 最後の応答の本文。
        content: Option<String>,
        /// serde の検証で出たエラー。
        reason: String,
    },
}

impl fmt::Display for JsonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Chat(e) => write!(f, "{e}"),
            Self::Invalid { reason, .. } => {
                write!(f, "the model did not return the expected JSON: {reason}")
            }
        }
    }
}

impl std::error::Error for JsonError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Chat(e) => Some(e),
            Self::Invalid { .. } => None,
        }
    }
}

impl From<ChatError> for JsonError {
    fn from(e: ChatError) -> Self {
        Self::Chat(e)
    }
}

impl Router {
    pub fn new(config: &Config) -> Self {
        Self {
            models: config.models.clone(),
            reasoning_effort: config.reasoning_effort.clone(),
            capabilities: config.capabilities.clone(),
        }
    }

    /// `role` に使うモデル ID。
    pub fn model(&self, role: Role) -> &str {
        match role {
            Role::Light => &self.models.light,
            Role::Generate => &self.models.generate,
            Role::Reason => &self.models.reason,
        }
    }

    /// `role` で送る `reasoning_effort`。`[capabilities]` が偽なら `None`（送らない）。
    /// 受け付けないモデルに送るとリクエストごと拒否されることがあるため（PRD §11）。
    pub fn reasoning_effort(&self, role: Role) -> Option<&str> {
        if !self.capabilities(role).reasoning_effort {
            return None;
        }
        Some(match role {
            Role::Light => &self.reasoning_effort.light,
            Role::Generate => &self.reasoning_effort.generate,
            Role::Reason => &self.reasoning_effort.reason,
        })
    }

    /// `role` のモデル ID と `reasoning_effort` を入れたリクエスト。ツールと `response_format` は空。
    pub fn request(&self, role: Role, messages: Vec<Message>) -> ChatRequest {
        ChatRequest {
            model: self.model(role).to_owned(),
            messages,
            tools: Vec::new(),
            tool_choice: None,
            response_format: None,
            reasoning_effort: self.reasoning_effort(role).map(str::to_owned),
        }
    }

    /// `schema` に従う JSON で答えさせ、`T` に読んで返す。
    ///
    /// `[capabilities]` で `json_schema` が真なら `response_format: json_schema` で送り、応答を
    /// 検証する。偽なら `json_object` で送り、スキーマを system メッセージで伝え、応答が `T` に
    /// 読めなければ、誤った応答と検証のエラーを続けて 1 回だけ直させる（PRD §8.7）。
    /// `json_schema` の経路は `doctor` が遵守を確かめた上で使うので、修復しない。
    pub fn chat_json<T: DeserializeOwned>(
        &self,
        client: &dyn ChatClient,
        role: Role,
        messages: Vec<Message>,
        name: &str,
        schema: &Value,
    ) -> Result<T, JsonError> {
        if self.capabilities(role).json_schema {
            let mut request = self.request(role, messages);
            request.response_format = Some(ResponseFormat::JsonSchema {
                name: name.to_owned(),
                schema: schema.clone(),
            });
            let content = client.chat(&request)?.content;
            return parse(&content).map_err(|reason| JsonError::Invalid { content, reason });
        }

        let mut request = self.request(role, with_schema_prompt(messages, schema));
        request.response_format = Some(ResponseFormat::JsonObject);
        let content = client.chat(&request)?.content;
        let reason = match parse(&content) {
            Ok(value) => return Ok(value),
            Err(reason) => reason,
        };

        request.messages.push(Message::Assistant {
            content: content.clone(),
            tool_calls: Vec::new(),
        });
        request.messages.push(Message::User {
            content: format!(
                "Your previous reply was not valid: {reason}\n\
                 Reply again with only a JSON object that conforms to the JSON Schema given above."
            ),
        });
        let content = client.chat(&request)?.content;
        parse(&content).map_err(|reason| JsonError::Invalid { content, reason })
    }

    fn capabilities(&self, role: Role) -> &RoleCapabilities {
        match role {
            Role::Light => &self.capabilities.light,
            Role::Generate => &self.capabilities.generate,
            Role::Reason => &self.capabilities.reason,
        }
    }
}

/// 応答の本文を `T` に読む。読めなければ、モデルに返せるエラーの文。
fn parse<T: DeserializeOwned>(content: &Option<String>) -> Result<T, String> {
    let text = content.as_deref().ok_or("the reply had no content")?;
    serde_json::from_str(text).map_err(|e| e.to_string())
}

/// スキーマを伝える指示を system メッセージに入れる。先頭が system メッセージならその末尾に足し、
/// 無ければ先頭に置く。system メッセージを先頭の 1 つに限るチャットテンプレートがあるため。
fn with_schema_prompt(mut messages: Vec<Message>, schema: &Value) -> Vec<Message> {
    let instruction =
        format!("Reply with only a JSON object that conforms to this JSON Schema:\n{schema}");
    match messages.first_mut() {
        Some(Message::System { content }) => {
            content.push_str("\n\n");
            content.push_str(&instruction);
        }
        _ => messages.insert(
            0,
            Message::System {
                content: instruction,
            },
        ),
    }
    messages
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::FakeChatClient;
    use crate::llm::types::ChatResponse;
    use serde::Deserialize;
    use serde_json::json;

    const ROLES: [Role; 3] = [Role::Light, Role::Generate, Role::Reason];

    #[derive(Debug, PartialEq, Deserialize)]
    struct Verdict {
        known: bool,
    }

    fn schema() -> Value {
        json!({
            "type": "object",
            "properties": {"known": {"type": "boolean"}},
            "required": ["known"],
        })
    }

    fn config() -> Config {
        Config {
            models: Models {
                light: "light-model".to_owned(),
                generate: "generate-model".to_owned(),
                reason: "reason-model".to_owned(),
            },
            reasoning_effort: ReasoningEffort {
                light: "low".to_owned(),
                generate: "medium".to_owned(),
                reason: "high".to_owned(),
            },
            ..Config::default()
        }
    }

    fn with_capability(edit: impl Fn(&mut RoleCapabilities)) -> Config {
        let mut config = config();
        edit(&mut config.capabilities.light);
        edit(&mut config.capabilities.generate);
        edit(&mut config.capabilities.reason);
        config
    }

    fn user(text: &str) -> Vec<Message> {
        vec![Message::User {
            content: text.to_owned(),
        }]
    }

    fn reply(text: &str) -> ChatResponse {
        ChatResponse {
            content: Some(text.to_owned()),
            tool_calls: Vec::new(),
        }
    }

    fn system_text(request: &ChatRequest) -> String {
        request
            .messages
            .iter()
            .filter_map(|message| match message {
                Message::System { content } => Some(content.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn request_uses_the_model_id_configured_for_each_role() {
        let router = Router::new(&config());
        let models: Vec<String> = ROLES
            .iter()
            .map(|&role| router.request(role, user("hi")).model)
            .collect();
        assert_eq!(models, ["light-model", "generate-model", "reason-model"]);
    }

    #[test]
    fn request_has_no_reasoning_effort_when_capabilities_is_false() {
        let fake = FakeChatClient::new();
        let router = Router::new(&config());
        for role in ROLES {
            fake.push_chat(Ok(reply("ok")));
            fake.chat(&router.request(role, user("hi"))).unwrap();
        }

        let sent: Vec<Option<String>> = fake
            .requests()
            .into_iter()
            .map(|request| request.reasoning_effort)
            .collect();
        assert_eq!(sent, [None, None, None]);
    }

    #[test]
    fn request_carries_the_configured_reasoning_effort_when_capabilities_is_true() {
        let router = Router::new(&with_capability(|c| c.reasoning_effort = true));
        let sent: Vec<Option<String>> = ROLES
            .iter()
            .map(|&role| router.request(role, user("hi")).reasoning_effort)
            .collect();
        assert_eq!(
            sent,
            [
                Some("low".to_owned()),
                Some("medium".to_owned()),
                Some("high".to_owned())
            ]
        );
    }

    #[test]
    fn reasoning_effort_follows_the_capability_of_its_own_role() {
        let mut config = config();
        config.capabilities.reason.reasoning_effort = true;
        let router = Router::new(&config);

        assert_eq!(router.reasoning_effort(Role::Light), None);
        assert_eq!(router.reasoning_effort(Role::Generate), None);
        assert_eq!(router.reasoning_effort(Role::Reason), Some("high"));
    }

    #[test]
    fn chat_json_with_json_schema_sends_the_schema_as_response_format() {
        let fake = FakeChatClient::new();
        fake.push_chat(Ok(reply(r#"{"known": true}"#)));
        let router = Router::new(&with_capability(|c| c.json_schema = true));

        let verdict: Verdict = router
            .chat_json(&fake, Role::Light, user("judge"), "verdict", &schema())
            .unwrap();

        assert_eq!(verdict, Verdict { known: true });
        let requests = fake.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].model, "light-model");
        assert_eq!(
            requests[0].response_format,
            Some(ResponseFormat::JsonSchema {
                name: "verdict".to_owned(),
                schema: schema(),
            })
        );
    }

    #[test]
    fn chat_json_with_json_schema_does_not_repair_an_invalid_reply() {
        let fake = FakeChatClient::new();
        fake.push_chat(Ok(reply("not json")));
        fake.push_chat(Ok(reply(r#"{"known": true}"#)));
        let router = Router::new(&with_capability(|c| c.json_schema = true));

        let result: Result<Verdict, _> =
            router.chat_json(&fake, Role::Light, user("judge"), "verdict", &schema());

        assert!(
            matches!(result, Err(JsonError::Invalid { .. })),
            "{result:?}"
        );
        assert_eq!(fake.requests().len(), 1);
    }

    #[test]
    fn chat_json_without_json_schema_sends_json_object_with_the_schema_in_the_prompt() {
        let fake = FakeChatClient::new();
        fake.push_chat(Ok(reply(r#"{"known": false}"#)));
        let router = Router::new(&config());

        let verdict: Verdict = router
            .chat_json(&fake, Role::Reason, user("judge"), "verdict", &schema())
            .unwrap();

        assert_eq!(verdict, Verdict { known: false });
        let requests = fake.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].model, "reason-model");
        assert_eq!(
            requests[0].response_format,
            Some(ResponseFormat::JsonObject)
        );
        assert!(
            matches!(requests[0].messages[0], Message::System { .. }),
            "the schema is not in a leading system message: {:?}",
            requests[0].messages
        );
        let system = system_text(&requests[0]);
        assert!(system.contains(&schema().to_string()), "{system}");
        assert!(
            requests[0].messages.contains(&Message::User {
                content: "judge".to_owned()
            }),
            "{:?}",
            requests[0].messages
        );
    }

    #[test]
    fn chat_json_without_json_schema_merges_the_schema_into_a_leading_system_message() {
        let fake = FakeChatClient::new();
        fake.push_chat(Ok(reply(r#"{"known": true}"#)));
        let router = Router::new(&config());
        let messages = vec![
            Message::System {
                content: "You judge answers.".to_owned(),
            },
            Message::User {
                content: "judge".to_owned(),
            },
        ];

        let _: Verdict = router
            .chat_json(&fake, Role::Light, messages, "verdict", &schema())
            .unwrap();

        let sent = &fake.requests()[0].messages;
        let systems = sent
            .iter()
            .filter(|message| matches!(message, Message::System { .. }))
            .count();
        assert_eq!(systems, 1, "{sent:?}");
        let Message::System { content } = &sent[0] else {
            panic!("the first message is not a system message: {sent:?}");
        };
        assert!(content.starts_with("You judge answers."), "{content}");
        assert!(content.contains(&schema().to_string()), "{content}");
    }

    #[test]
    fn chat_json_without_json_schema_repairs_an_invalid_reply_once() {
        let fake = FakeChatClient::new();
        fake.push_chat(Ok(reply(r#"{"known": "yes"}"#)));
        fake.push_chat(Ok(reply(r#"{"known": true}"#)));
        let router = Router::new(&config());

        let verdict: Verdict = router
            .chat_json(&fake, Role::Generate, user("judge"), "verdict", &schema())
            .unwrap();

        assert_eq!(verdict, Verdict { known: true });
        let requests = fake.requests();
        assert_eq!(requests.len(), 2);
        let repair = &requests[1];
        assert_eq!(repair.model, "generate-model");
        assert_eq!(repair.response_format, Some(ResponseFormat::JsonObject));
        // 修復の依頼は、最初の依頼に誤った応答と検証のエラーを続けたもの
        let first = &requests[0].messages;
        assert_eq!(&repair.messages[..first.len()], &first[..]);
        assert_eq!(
            repair.messages[first.len()],
            Message::Assistant {
                content: Some(r#"{"known": "yes"}"#.to_owned()),
                tool_calls: Vec::new(),
            }
        );
        let Message::User { content } = &repair.messages[first.len() + 1] else {
            panic!("the repair request does not end with a user message");
        };
        assert!(content.contains("invalid type"), "{content}");
        assert_eq!(repair.messages.len(), first.len() + 2);
    }

    #[test]
    fn chat_json_without_json_schema_gives_up_after_one_repair() {
        let fake = FakeChatClient::new();
        fake.push_chat(Ok(reply("not json")));
        fake.push_chat(Ok(reply("still not json")));
        fake.push_chat(Ok(reply(r#"{"known": true}"#)));
        let router = Router::new(&config());

        let result: Result<Verdict, _> =
            router.chat_json(&fake, Role::Light, user("judge"), "verdict", &schema());

        let Err(JsonError::Invalid { content, .. }) = result else {
            panic!("expected Invalid, got {result:?}");
        };
        assert_eq!(content.as_deref(), Some("still not json"));
        assert_eq!(fake.requests().len(), 2);
    }

    #[test]
    fn chat_json_without_json_schema_repairs_a_reply_with_no_content() {
        let fake = FakeChatClient::new();
        fake.push_chat(Ok(ChatResponse {
            content: None,
            tool_calls: Vec::new(),
        }));
        fake.push_chat(Ok(reply(r#"{"known": true}"#)));
        let router = Router::new(&config());

        let verdict: Verdict = router
            .chat_json(&fake, Role::Light, user("judge"), "verdict", &schema())
            .unwrap();

        assert_eq!(verdict, Verdict { known: true });
        assert_eq!(fake.requests().len(), 2);
    }

    #[test]
    fn chat_json_returns_a_chat_failure_without_repairing() {
        let fake = FakeChatClient::new();
        fake.push_chat(Err(ChatError::FakeExhausted));
        fake.push_chat(Ok(reply(r#"{"known": true}"#)));
        let router = Router::new(&config());

        let result: Result<Verdict, _> =
            router.chat_json(&fake, Role::Light, user("judge"), "verdict", &schema());

        assert!(
            matches!(result, Err(JsonError::Chat(ChatError::FakeExhausted))),
            "{result:?}"
        );
        assert_eq!(fake.requests().len(), 1);
    }

    #[test]
    fn chat_json_sends_reasoning_effort_only_when_capabilities_is_true() {
        let fake = FakeChatClient::new();
        fake.push_chat(Ok(reply(r#"{"known": true}"#)));
        fake.push_chat(Ok(reply(r#"{"known": true}"#)));

        let _: Verdict = Router::new(&config())
            .chat_json(&fake, Role::Reason, user("judge"), "verdict", &schema())
            .unwrap();
        let _: Verdict = Router::new(&with_capability(|c| c.reasoning_effort = true))
            .chat_json(&fake, Role::Reason, user("judge"), "verdict", &schema())
            .unwrap();

        let sent: Vec<Option<String>> = fake
            .requests()
            .into_iter()
            .map(|request| request.reasoning_effort)
            .collect();
        assert_eq!(sent, [None, Some("high".to_owned())]);
    }
}
