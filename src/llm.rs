//! OpenAI 互換の chat API の呼び出し。agent と `molt doctor` は [`ChatClient`] だけを見るので、
//! テストでは [`FakeChatClient`] に差し替えればネットワーク無しで動く（PRD §8.1・§9）。

pub mod fake;
pub(crate) mod fake_script;
pub(crate) mod interrupt;
pub mod openai;
pub mod router;
pub(crate) mod stream;
pub mod types;

pub use fake::FakeChatClient;
pub(crate) use interrupt::Interrupt;
pub use openai::OpenAiClient;
pub use router::{JsonError, Role, Router};
pub use types::{
    ChatError, ChatRequest, ChatResponse, InvalidArguments, Message, ResponseFormat, StreamEvent,
    ToolCall, ToolChoice, ToolDefinition, UnknownTool,
};

/// chat API のエンドポイント。
///
/// 同期にしている。呼び出し側の本線は rustyline で入力を待つ同期の loop で、
/// 同時に飛ばすリクエストも 1 つに限るため、async にしても並行に走らせるものが無い。
/// `molt init` の判定は数本を並列に送るので（PRD UC1）、スレッドの間で共有できるようにする。
pub trait ChatClient: Send + Sync {
    /// `request` を送り、モデルの応答を返す。
    ///
    /// 応答の tool_calls の名前は検査せずに返す。呼び出し側が [`types::check_tool_name`] で確かめる。
    fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, ChatError>;

    /// `request` を送り、応答の文章を届いたそばに 1 行ずつ `on_event` に渡してから、応答を返す
    /// （PRD §8.1）。流して受けられないクライアントは、応答がそろってから文章の行をまとめて渡す。
    fn chat_streamed(
        &self,
        request: &ChatRequest,
        on_event: &mut dyn FnMut(StreamEvent),
    ) -> Result<ChatResponse, ChatError> {
        let response = self.chat(request)?;
        if let Some(content) = &response.content {
            for line in content.lines() {
                on_event(StreamEvent::Line(line.to_owned()));
            }
        }
        Ok(response)
    }

    /// エンドポイントが提供するモデルの ID を返す。`molt doctor` が ID の存在を確かめるのに使う。
    fn models(&self) -> Result<Vec<String>, ChatError>;
}
