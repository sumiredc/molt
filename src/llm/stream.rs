//! 流して受ける応答（server-sent events）を組み立てる（PRD §8.1・§8.7）。
//!
//! 1 行ずつ [`Assembler::feed`] に渡す。文章（`content`）は改行まで届いた行から
//! [`StreamEvent::Line`] にして返し、推論（`reasoning_content`・`reasoning`）は
//! [`StreamEvent::Reasoning`] だけを返す。ツールの呼び出しは、断片を `index` ごとに
//! つないで組み立てる。`finish_reason` が来る前に終わったストリームは失敗とする。
//!
//! API キーは、渡す行と組み立てた応答から伏せる。行は改行で区切るので、キーが 2 つの断片に
//! またがっても、1 行の中で伏せられる。

use std::collections::BTreeMap;

use serde::Deserialize;

use super::types::{ChatError, ChatResponse, StreamEvent, ToolCall};

/// 流して受けた応答を組み立てる。
#[derive(Debug, Default)]
pub(crate) struct Assembler {
    /// 伏せる API キー。
    key: Option<String>,
    /// これまでに届いた文章のすべて。
    content: String,
    /// 届いた文章のうち、まだ改行が来ていない行。
    pending: String,
    /// ツールの呼び出しを `index` ごとに。
    calls: BTreeMap<u64, PartialCall>,
    /// `finish_reason` が来たか。
    finished: bool,
    /// `data: [DONE]` が来たか。
    done: bool,
}

#[derive(Debug, Default)]
struct PartialCall {
    id: String,
    name: String,
    arguments: String,
}

#[derive(Deserialize)]
struct WireChunk {
    #[serde(default)]
    choices: Vec<WireChoice>,
}

#[derive(Deserialize)]
struct WireChoice {
    #[serde(default)]
    delta: WireDelta,
    finish_reason: Option<String>,
}

#[derive(Default, Deserialize)]
struct WireDelta {
    content: Option<String>,
    reasoning_content: Option<String>,
    reasoning: Option<String>,
    tool_calls: Option<Vec<WireToolCallDelta>>,
}

#[derive(Deserialize)]
struct WireToolCallDelta {
    #[serde(default)]
    index: u64,
    id: Option<String>,
    function: Option<WireFunctionDelta>,
}

#[derive(Deserialize)]
struct WireFunctionDelta {
    name: Option<String>,
    arguments: Option<serde_json::Value>,
}

impl Assembler {
    /// API キー `key`（無ければ `None`）を伏せる組み立て。
    pub(crate) fn new(key: Option<&str>) -> Self {
        Self {
            key: key.map(str::to_owned),
            ..Self::default()
        }
    }

    /// `[DONE]` が来て、これ以上読まなくてよいか。
    pub(crate) fn is_done(&self) -> bool {
        self.done
    }

    /// 受けた 1 行を読み、届けるものを返す。空の行とコメント（`:` で始まる行。キープアライブ）は
    /// 何も返さない。
    pub(crate) fn feed(&mut self, line: &str) -> Result<Vec<StreamEvent>, ChatError> {
        let line = line.trim_end_matches('\r');
        let Some(data) = line.strip_prefix("data:") else {
            return Ok(Vec::new());
        };
        let data = data.trim_start();
        if data == "[DONE]" {
            self.done = true;
            return Ok(Vec::new());
        }
        let chunk: WireChunk = serde_json::from_str(data)
            .map_err(|error| ChatError::InvalidResponse(self.redact(&error.to_string())))?;
        let mut events = Vec::new();
        for choice in chunk.choices {
            let delta = choice.delta;
            let reasoning = [delta.reasoning_content, delta.reasoning]
                .into_iter()
                .flatten()
                .any(|text| !text.is_empty());
            if reasoning {
                events.push(StreamEvent::Reasoning);
            }
            if let Some(text) = delta.content {
                self.content.push_str(&text);
                self.pending.push_str(&text);
                while let Some(at) = self.pending.find('\n') {
                    let line: String = self.pending.drain(..=at).collect();
                    events.push(StreamEvent::Line(self.redact(line.trim_end_matches('\n'))));
                }
            }
            for call in delta.tool_calls.unwrap_or_default() {
                let partial = self.calls.entry(call.index).or_default();
                if let Some(id) = call.id.filter(|id| !id.is_empty()) {
                    partial.id = id;
                }
                if let Some(function) = call.function {
                    if let Some(name) = function.name {
                        partial.name.push_str(&name);
                    }
                    match function.arguments {
                        Some(serde_json::Value::String(piece)) => {
                            partial.arguments.push_str(&piece)
                        }
                        Some(serde_json::Value::Null) | None => {}
                        // 断片でなく、まとめた値で送る接続先
                        Some(whole) => partial.arguments = whole.to_string(),
                    }
                }
            }
            if choice.finish_reason.is_some() {
                self.finished = true;
            }
        }
        Ok(events)
    }

    /// ストリームの終わり。改行の来なかった最後の行を返し、組み立てた応答を返す。
    /// `finish_reason` が来ていなければ失敗。
    pub(crate) fn finish(mut self) -> Result<(Vec<StreamEvent>, ChatResponse), ChatError> {
        if !self.finished {
            return Err(ChatError::InvalidResponse(
                "the stream ended before its finish_reason".to_owned(),
            ));
        }
        let mut events = Vec::new();
        if !self.pending.is_empty() {
            let last = std::mem::take(&mut self.pending);
            events.push(StreamEvent::Line(self.redact(&last)));
        }
        let content = (!self.content.is_empty()).then(|| self.redact(&self.content));
        let tool_calls = std::mem::take(&mut self.calls)
            .into_values()
            .map(|call| ToolCall {
                id: call.id,
                name: call.name,
                arguments: call.arguments,
            })
            .collect();
        Ok((
            events,
            ChatResponse {
                content,
                tool_calls,
            },
        ))
    }

    fn redact(&self, text: &str) -> String {
        match &self.key {
            Some(key) => text.replace(key.as_str(), "<redacted>"),
            None => text.to_owned(),
        }
    }
}

#[cfg(test)]
mod tests;
