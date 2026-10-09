//! テストと `--fake` で使う [`ChatClient`]。積んだ応答を順に返し、送られたリクエストを記録する。

use std::cell::RefCell;
use std::collections::VecDeque;

use super::ChatClient;
use super::types::{ChatError, ChatRequest, ChatResponse};

/// 積んだ応答を順に返す [`ChatClient`]。ネットワークに出ない。
///
/// 応答は積んだまま返す。tool_calls の名前の検査は実クライアントの応答と同じく、呼び出し側が
/// [`check_tool_name`](super::types::check_tool_name) で行う。
#[derive(Debug, Default)]
pub struct FakeChatClient {
    chat_responses: RefCell<VecDeque<Result<ChatResponse, ChatError>>>,
    models_responses: RefCell<VecDeque<Result<Vec<String>, ChatError>>>,
    requests: RefCell<Vec<ChatRequest>>,
}

impl FakeChatClient {
    pub fn new() -> Self {
        Self::default()
    }

    /// 次の `chat` の呼び出しで返すものを、キューの末尾に積む。
    pub fn push_chat(&self, response: Result<ChatResponse, ChatError>) {
        self.chat_responses.borrow_mut().push_back(response);
    }

    /// 次の `models` の呼び出しで返すものを、キューの末尾に積む。
    pub fn push_models(&self, response: Result<Vec<String>, ChatError>) {
        self.models_responses.borrow_mut().push_back(response);
    }

    /// これまでに `chat` に渡されたリクエスト。送られた順。
    pub fn requests(&self) -> Vec<ChatRequest> {
        self.requests.borrow().clone()
    }
}

impl ChatClient for FakeChatClient {
    /// 積んだ応答を先頭から 1 つ返す。積んだものが無ければ [`ChatError::FakeExhausted`]。
    fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, ChatError> {
        self.requests.borrow_mut().push(request.clone());
        self.chat_responses
            .borrow_mut()
            .pop_front()
            .unwrap_or(Err(ChatError::FakeExhausted))
    }

    /// 積んだモデル ID の一覧を先頭から 1 つ返す。積んだものが無ければ [`ChatError::FakeExhausted`]。
    fn models(&self) -> Result<Vec<String>, ChatError> {
        self.models_responses
            .borrow_mut()
            .pop_front()
            .unwrap_or(Err(ChatError::FakeExhausted))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::types::{Message, ToolCall};

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
