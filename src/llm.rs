//! OpenAI 互換の chat API の呼び出し。agent と `molt doctor` は [`ChatClient`] だけを見るので、
//! テストでは [`FakeChatClient`] に差し替えればネットワーク無しで動く（PRD §8.1・§9）。

pub mod fake;
pub mod router;
pub mod types;

pub use fake::FakeChatClient;
pub use router::{JsonError, Role, Router};
pub use types::{
    ChatError, ChatRequest, ChatResponse, InvalidArguments, Message, ResponseFormat, ToolCall,
    ToolChoice, ToolDefinition, UnknownTool,
};

/// chat API のエンドポイント。
///
/// 同期にしている。呼び出し側の本線は rustyline と dialoguer で入力を待つ同期の loop で、
/// 同時に飛ばすリクエストも 1 つに限るため、async にしても並行に走らせるものが無い。
pub trait ChatClient {
    /// `request` を送り、モデルの応答を返す。
    ///
    /// 応答の tool_calls の名前は検査せずに返す。呼び出し側が [`types::check_tool_name`] で確かめる。
    fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, ChatError>;

    /// エンドポイントが提供するモデルの ID を返す。`molt doctor` が ID の存在を確かめるのに使う。
    fn models(&self) -> Result<Vec<String>, ChatError>;
}
