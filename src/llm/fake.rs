//! テストと `--fake` で使う [`ChatClient`]。積んだ応答を順に返し、送られたリクエストを記録する。
//! 並列に送られる依頼（`molt init` の判定）には、依頼の文字列で選ぶ応答を積む。

use std::collections::VecDeque;
use std::sync::Mutex;

use super::ChatClient;
use super::types::{ChatError, ChatRequest, ChatResponse, Message};

/// 積んだ応答を順に返す [`ChatClient`]。ネットワークに出ない。
///
/// 応答は積んだまま返す。tool_calls の名前の検査は実クライアントの応答と同じく、呼び出し側が
/// [`check_tool_name`](super::types::check_tool_name) で行う。
#[derive(Debug, Default)]
pub struct FakeChatClient {
    chat_responses: Mutex<VecDeque<Result<ChatResponse, ChatError>>>,
    /// 依頼の文字列で選ぶ応答。鍵の文字列を含む依頼に、積んだ順で最初のものを 1 回だけ返す。
    keyed_responses: Mutex<Vec<(String, Result<ChatResponse, ChatError>)>>,
    models_responses: Mutex<VecDeque<Result<Vec<String>, ChatError>>>,
    requests: Mutex<Vec<ChatRequest>>,
}

impl FakeChatClient {
    pub fn new() -> Self {
        Self::default()
    }

    /// 次の `chat` の呼び出しで返すものを、キューの末尾に積む。
    pub fn push_chat(&self, response: Result<ChatResponse, ChatError>) {
        lock(&self.chat_responses).push_back(response);
    }

    /// メッセージのどれかが `key` を含む `chat` の依頼に返すものを積む。順に返す応答より先に選ぶ。
    /// 並列に送られて届く順が決まらない依頼を、中身で答え分けるため。
    pub fn push_chat_when(
        &self,
        key: impl Into<String>,
        response: Result<ChatResponse, ChatError>,
    ) {
        lock(&self.keyed_responses).push((key.into(), response));
    }

    /// 次の `models` の呼び出しで返すものを、キューの末尾に積む。
    pub fn push_models(&self, response: Result<Vec<String>, ChatError>) {
        lock(&self.models_responses).push_back(response);
    }

    /// これまでに `chat` に渡されたリクエスト。届いた順。
    pub fn requests(&self) -> Vec<ChatRequest> {
        lock(&self.requests).clone()
    }

    /// `request` のメッセージのどれかが含む鍵の応答のうち、積んだ順で最初のものを取り出す。
    fn take_keyed(&self, request: &ChatRequest) -> Option<Result<ChatResponse, ChatError>> {
        let mut keyed = lock(&self.keyed_responses);
        let at = keyed.iter().position(|(key, _)| {
            request
                .messages
                .iter()
                .any(|message| text_of(message).contains(key.as_str()))
        })?;
        Some(keyed.remove(at).1)
    }
}

/// メッセージの本文。ツール呼び出しだけの assistant のメッセージは空。
fn text_of(message: &Message) -> &str {
    match message {
        Message::System { content } | Message::User { content } | Message::Tool { content, .. } => {
            content
        }
        Message::Assistant { content, .. } => content.as_deref().unwrap_or(""),
    }
}

/// テストの別のスレッドが panic しても、積んだものはそのまま使う。
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl ChatClient for FakeChatClient {
    /// 依頼に合う鍵の応答があればそれを、無ければ順に積んだ応答を先頭から 1 つ返す。どちらも
    /// 無ければ [`ChatError::FakeExhausted`]。
    fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, ChatError> {
        lock(&self.requests).push(request.clone());
        if let Some(response) = self.take_keyed(request) {
            return response;
        }
        lock(&self.chat_responses)
            .pop_front()
            .unwrap_or(Err(ChatError::FakeExhausted))
    }

    /// 積んだモデル ID の一覧を先頭から 1 つ返す。積んだものが無ければ [`ChatError::FakeExhausted`]。
    fn models(&self) -> Result<Vec<String>, ChatError> {
        lock(&self.models_responses)
            .pop_front()
            .unwrap_or(Err(ChatError::FakeExhausted))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::types::ToolCall;

    fn request(text: &str) -> ChatRequest {
        ChatRequest {
            model: "model".to_owned(),
            messages: vec![Message::User {
                content: text.to_owned(),
            }],
            tools: Vec::new(),
            tool_choice: None,
            response_format: None,
            reasoning_effort: None,
            stream: false,
        }
    }

    fn reply(text: &str) -> ChatResponse {
        ChatResponse {
            content: Some(text.to_owned()),
            tool_calls: Vec::new(),
        }
    }

    #[test]
    fn chat_returns_queued_responses_in_order() {
        let fake = FakeChatClient::new();
        fake.push_chat(Ok(reply("first")));
        fake.push_chat(Ok(reply("second")));

        assert_eq!(fake.chat(&request("a")).unwrap(), reply("first"));
        assert_eq!(fake.chat(&request("b")).unwrap(), reply("second"));
    }

    #[test]
    fn chat_records_sent_requests_in_order() {
        let fake = FakeChatClient::new();
        fake.push_chat(Ok(reply("first")));
        fake.push_chat(Ok(reply("second")));

        fake.chat(&request("a")).unwrap();
        fake.chat(&request("b")).unwrap();

        assert_eq!(fake.requests(), [request("a"), request("b")]);
    }

    #[test]
    fn chat_returns_a_queued_error_in_its_turn() {
        let fake = FakeChatClient::new();
        fake.push_chat(Err(ChatError::FakeExhausted));
        fake.push_chat(Ok(reply("after the error")));

        assert!(matches!(
            fake.chat(&request("a")),
            Err(ChatError::FakeExhausted)
        ));
        assert_eq!(fake.chat(&request("b")).unwrap(), reply("after the error"));
    }

    #[test]
    fn chat_after_the_queue_runs_out_is_exhausted_and_still_recorded() {
        let fake = FakeChatClient::new();
        fake.push_chat(Ok(reply("only")));
        fake.chat(&request("a")).unwrap();

        assert!(matches!(
            fake.chat(&request("b")),
            Err(ChatError::FakeExhausted)
        ));
        assert_eq!(fake.requests(), [request("a"), request("b")]);
    }

    #[test]
    fn chat_returns_a_tool_call_with_an_unknown_name_unchanged_so_its_id_survives() {
        let fake = FakeChatClient::new();
        let queued = ChatResponse {
            content: None,
            tool_calls: vec![ToolCall {
                id: "call_7".to_owned(),
                name: "delete_file".to_owned(),
                arguments: "{}".to_owned(),
            }],
        };
        fake.push_chat(Ok(queued.clone()));

        assert_eq!(fake.chat(&request("a")).unwrap(), queued);
    }

    #[test]
    fn a_keyed_response_answers_the_first_request_containing_its_key_before_the_queue() {
        let fake = FakeChatClient::new();
        fake.push_chat(Ok(reply("in order")));
        fake.push_chat_when("beta", Ok(reply("for beta")));
        fake.push_chat_when("alpha", Ok(reply("for alpha")));

        assert_eq!(
            fake.chat(&request("about alpha")).unwrap(),
            reply("for alpha")
        );
        assert_eq!(
            fake.chat(&request("about alpha")).unwrap(),
            reply("in order")
        );
        assert_eq!(
            fake.chat(&request("about beta")).unwrap(),
            reply("for beta")
        );
        assert!(matches!(
            fake.chat(&request("about beta")),
            Err(ChatError::FakeExhausted)
        ));
    }

    #[test]
    fn models_returns_queued_lists_in_order() {
        let fake = FakeChatClient::new();
        fake.push_models(Ok(vec!["a".to_owned()]));
        fake.push_models(Ok(vec!["b".to_owned(), "c".to_owned()]));

        assert_eq!(fake.models().unwrap(), ["a"]);
        assert_eq!(fake.models().unwrap(), ["b", "c"]);
    }

    #[test]
    fn models_after_the_queue_runs_out_is_exhausted() {
        let fake = FakeChatClient::new();
        assert!(matches!(fake.models(), Err(ChatError::FakeExhausted)));
    }
}
