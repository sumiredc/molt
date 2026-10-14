//! `molt doctor`: 設定した接続先のモデルが何を受け付けるかを役割ごとに確かめて表にし、
//! `[capabilities]` を `config.toml` に書く（PRD §8.7・§11）。
//!
//! 確かめる 5 項目は、ID の存在（`models()`）、素の chat、`tool_choice: required`、
//! `response_format: json_schema` の遵守、`reasoning_effort` の受け付け。
//! 接続の失敗は機能の欠落と分けて報告し、`[capabilities]` を書き換えない。
//! 落ちているだけのエンドポイントを「非対応」と記録すると、`[capabilities]` が誤るため。

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde::Deserialize;
use serde_json::json;
use toml_edit::{DocumentMut, InlineTable, Item, Table, TableLike};

use crate::config::{self, Capabilities, Config, RoleCapabilities};
use crate::error::{Error, Result};
use crate::llm::types::{check_tool_name, parse_arguments};
use crate::llm::{
    ChatClient, ChatError, ChatRequest, ChatResponse, Message, OpenAiClient, ResponseFormat, Role,
    Router, ToolChoice, ToolDefinition,
};

const ROLES: [Role; 3] = [Role::Light, Role::Generate, Role::Reason];

/// `tool_choice: required` の確かめに渡すツールの名前。
const TOOL_NAME: &str = "record_answer";

/// `molt doctor` の入口。`config.toml` の `[api]` の接続先に対して確かめる。
pub(crate) fn main() -> ExitCode {
    let path = match config::molt_home(|name| std::env::var_os(name)) {
        Ok(home) => config::config_path(&home),
        Err(e) => return fail(&e),
    };
    let config = match Config::load(&path) {
        Ok(config) => config,
        Err(e) => return fail(&e),
    };
    let client = match OpenAiClient::new(&config.api, |name| std::env::var(name).ok()) {
        Ok(client) => client,
        Err(error) => {
            // クライアントを作れない（API キーの環境変数が無いなど）ときも、確かめられていないので書き換えない
            let unchecked = Unchecked { role: None, error };
            let _ = report_unchecked(&path, &unchecked, &mut io::stderr().lock());
            return ExitCode::FAILURE;
        }
    };
    match run(&path, &config, &client, &mut io::stdout().lock()) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => fail(&e),
    }
}

fn fail(error: &Error) -> ExitCode {
    eprintln!("{error}");
    ExitCode::FAILURE
}

/// `config` の役割ごとに確かめ、結果を `out` に書き、`path` の `[capabilities]` を書き換える。
///
/// 接続に失敗したか設定が誤っていたら、その旨を書いて `Ok(false)` を返し、`path` には触れない。
fn run(path: &Path, config: &Config, client: &dyn ChatClient, out: &mut dyn Write) -> Result<bool> {
    let io_error = |source| Error::Io {
        path: PathBuf::from("<stdout>"),
        source,
    };
    match diagnose(config, client) {
        Err(unchecked) => {
            report_unchecked(path, &unchecked, out).map_err(io_error)?;
            Ok(false)
        }
        Ok(reports) => {
            write!(out, "{}", render(&reports)).map_err(io_error)?;
            write_capabilities(path, config, &capabilities(&reports))?;
            writeln!(out, "wrote [capabilities] to {}", path.display()).map_err(io_error)?;
            Ok(true)
        }
    }
}

/// 確かめられなかった理由と、`[capabilities]` を書き換えていないことを書く。
fn report_unchecked(path: &Path, unchecked: &Unchecked, out: &mut dyn Write) -> io::Result<()> {
    writeln!(out, "{unchecked}")?;
    writeln!(out, "[capabilities] in {} was not changed", path.display())
}

/// 1 項目の結果。
#[derive(Debug, Clone, PartialEq)]
enum Check {
    Pass,
    /// 確かめたが受け付けなかった。理由を持つ。
    Fail(String),
    /// ID が無いので確かめなかった。
    Skipped,
}

impl Check {
    fn passed(&self) -> bool {
        matches!(self, Self::Pass)
    }
}

/// 1 つの役割の結果。
#[derive(Debug)]
struct RoleReport {
    role: Role,
    model: String,
    /// 確かめに送った `reasoning_effort` の値。
    effort: String,
    exists: Check,
    chat: Check,
    tool_choice: Check,
    json_schema: Check,
    reasoning_effort: Check,
    /// 流して受けた応答から、文章とツールの呼び出しを組み立てられるか（PRD §8.7）。
    streaming: Check,
}

/// 確かめを続けられなかった理由（接続の失敗か設定の誤り）。どの役割を確かめている途中か
/// （クライアントの作成と `models()` なら `None`）と、エラー。
#[derive(Debug)]
struct Unchecked {
    role: Option<Role>,
    error: ChatError,
}

impl std::fmt::Display for Unchecked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if is_configuration_error(&self.error) {
            // 接続先が落ちているのではなく、利用者が config.toml か環境変数を直すものなので分けて出す
            return write!(f, "configuration error: {}", self.error);
        }
        match self.role {
            None => write!(f, "connection failed: {}", self.error),
            Some(role) => write!(
                f,
                "connection failed while checking {}: {}",
                role_name(role),
                self.error
            ),
        }
    }
}

/// API キーの環境変数が無い、キーを平文の HTTP で外に送ることになる、のように、
/// 接続先ではなく利用者の設定を直すべきエラーか。
fn is_configuration_error(error: &ChatError) -> bool {
    matches!(
        error,
        ChatError::MissingApiKey { .. } | ChatError::ApiKeyOverPlainHttp { .. }
    )
}

/// 機能の欠落ではなく、確かめを止めるエラー（接続の失敗か設定の誤り）か。
///
/// 届かない（`Request`）、タイムアウト（`TimedOut`）、認証の拒否（401・403）、混雑（408・429）、
/// エンドポイント側の障害（5xx）は、モデルが機能を持つかと関係なく起きるので、機能の判定に使わない。
/// 中断（`Interrupted`）は、doctor が Ctrl-C を受けるハンドラを入れないので起きない。来たら同じく止める。
fn is_connection_failure(error: &ChatError) -> bool {
    match error {
        ChatError::Request(_)
        | ChatError::TimedOut { .. }
        | ChatError::MissingApiKey { .. }
        | ChatError::ApiKeyOverPlainHttp { .. }
        | ChatError::Interrupted => true,
        ChatError::Status { status, .. } => {
            matches!(status, 401 | 403 | 408 | 429) || *status >= 500
        }
        ChatError::FakeExhausted | ChatError::InvalidResponse(_) => false,
    }
}

/// 役割ごとに 5 項目を確かめる。`models()` が返らないか、確かめる途中で接続に失敗したら、
/// それまでの結果を捨てて [`Unchecked`] を返す。
fn diagnose(
    config: &Config,
    client: &dyn ChatClient,
) -> std::result::Result<Vec<RoleReport>, Unchecked> {
    let available = client
        .models()
        .map_err(|error| Unchecked { role: None, error })?;
    let router = Router::new(config);
    ROLES
        .iter()
        .map(|&role| {
            check_role(
                client,
                role,
                router.model(role),
                effort(config, role),
                &available,
            )
            .map_err(|error| Unchecked {
                role: Some(role),
                error,
            })
        })
        .collect()
}

fn check_role(
    client: &dyn ChatClient,
    role: Role,
    model: &str,
    effort: &str,
    available: &[String],
) -> std::result::Result<RoleReport, ChatError> {
    let mut report = RoleReport {
        role,
        model: model.to_owned(),
        effort: effort.to_owned(),
        exists: Check::Pass,
        chat: Check::Skipped,
        tool_choice: Check::Skipped,
        json_schema: Check::Skipped,
        reasoning_effort: Check::Skipped,
        streaming: Check::Skipped,
    };
    if !available.iter().any(|id| id == model) {
        // 無い ID に送っても拒否されるだけなので、残りの 4 項目は確かめない
        report.exists = Check::Fail("not in the endpoint's model list".to_owned());
        return Ok(report);
    }

    // 各項目は 1 つの機能だけを足して送る。reasoning_effort は既存の [capabilities] に関係なく、
    // その項目でだけ送る
    report.chat = probe(client, &plain_request(model), judge_reply)?;
    report.tool_choice = probe(client, &tool_request(model), judge_tool_call)?;
    report.json_schema = probe(client, &json_schema_request(model), judge_json_schema)?;
    let mut request = plain_request(model);
    request.reasoning_effort = Some(effort.to_owned());
    report.reasoning_effort = probe(client, &request, judge_reply)?;
    // 流して受けた文章と、断片から組み立てたツールの呼び出しの両方が読めれば使える
    let mut request = plain_request(model);
    request.stream = true;
    let text = probe(client, &request, judge_reply)?;
    let mut request = tool_request(model);
    request.stream = true;
    let tool = probe(client, &request, judge_tool_call)?;
    report.streaming = if text.passed() { tool } else { text };
    Ok(report)
}

/// `request` を送り、応答を `judge` で判定する。接続の失敗だけを `Err` で返し、
/// それ以外のエラー（400 など）は機能を受け付けなかったものとして [`Check::Fail`] にする。
fn probe(
    client: &dyn ChatClient,
    request: &ChatRequest,
    judge: fn(ChatResponse) -> Check,
) -> std::result::Result<Check, ChatError> {
    match client.chat(request) {
        Ok(response) => Ok(judge(response)),
        Err(error) if is_connection_failure(&error) => Err(error),
        Err(error) => Ok(Check::Fail(error.to_string())),
    }
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

fn plain_request(model: &str) -> ChatRequest {
    request(model, "Reply with the single word OK.")
}

fn answer_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {"answer": {"type": "integer"}},
        "required": ["answer"],
        "additionalProperties": false,
    })
}

fn tool_request(model: &str) -> ChatRequest {
    let mut request = request(model, "What is 2 + 3? Record the answer with the tool.");
    request.tools = vec![answer_tool()];
    request.tool_choice = Some(ToolChoice::Required);
    request
}

fn answer_tool() -> ToolDefinition {
    ToolDefinition {
        name: TOOL_NAME.to_owned(),
        description: "Record the answer to the question.".to_owned(),
        parameters: answer_schema(),
    }
}

fn json_schema_request(model: &str) -> ChatRequest {
    let mut request = request(model, "What is 2 + 3? Reply in JSON.");
    request.response_format = Some(ResponseFormat::JsonSchema {
        name: "answer".to_owned(),
        schema: answer_schema(),
    });
    request
}

/// [`answer_schema`] に従う応答。スキーマの `additionalProperties: false` に合わせて、余分なキーを拒否する。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Answer {
    #[allow(dead_code)]
    answer: i64,
}

/// 空でない本文があれば受け付けたとみなす。
fn judge_reply(response: ChatResponse) -> Check {
    match response.content.as_deref().map(str::trim) {
        Some(text) if !text.is_empty() => Check::Pass,
        _ => Check::Fail("the reply had no content".to_owned()),
    }
}

/// 渡したツールの呼び出しがあり、どの呼び出しも名前が正しく引数がオブジェクトとして読めれば受け付けたとみなす。
fn judge_tool_call(response: ChatResponse) -> Check {
    if response.tool_calls.is_empty() {
        return Check::Fail("the reply had no tool call".to_owned());
    }
    let tools = [answer_tool()];
    for call in &response.tool_calls {
        if let Err(e) = check_tool_name(call, &tools) {
            return Check::Fail(e.to_string());
        }
        if let Err(e) = parse_arguments(call) {
            return Check::Fail(e.to_string());
        }
    }
    Check::Pass
}

/// 本文が [`answer_schema`] に従う JSON なら遵守したとみなす。
fn judge_json_schema(response: ChatResponse) -> Check {
    let Some(text) = response.content else {
        return Check::Fail("the reply had no content".to_owned());
    };
    match serde_json::from_str::<Answer>(&text) {
        Ok(_) => Check::Pass,
        Err(e) => Check::Fail(format!("the reply did not follow the schema: {e}")),
    }
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::Light => "light",
        Role::Generate => "generate",
        Role::Reason => "reason",
    }
}

fn effort(config: &Config, role: Role) -> &str {
    match role {
        Role::Light => &config.reasoning_effort.light,
        Role::Generate => &config.reasoning_effort.generate,
        Role::Reason => &config.reasoning_effort.reason,
    }
}

/// 結果から `[capabilities]` を作る。確かめて受け付けた項目だけを真にする。
fn capabilities(reports: &[RoleReport]) -> Capabilities {
    let mut capabilities = Capabilities::default();
    for report in reports {
        let role = match report.role {
            Role::Light => &mut capabilities.light,
            Role::Generate => &mut capabilities.generate,
            Role::Reason => &mut capabilities.reason,
        };
        *role = RoleCapabilities {
            tool_choice_required: report.tool_choice.passed(),
            json_schema: report.json_schema.passed(),
            reasoning_effort: report.reasoning_effort.passed(),
            streaming: report.streaming.passed(),
        };
    }
    capabilities
}

/// 結果の表と、受け付けなかった項目の理由。
fn render(reports: &[RoleReport]) -> String {
    let cell = |check: &Check| match check {
        Check::Pass => "ok",
        Check::Fail(_) => "no",
        Check::Skipped => "-",
    };
    let mut rows = vec![[
        "role".to_owned(),
        "model".to_owned(),
        "id".to_owned(),
        "chat".to_owned(),
        "tool_choice: required".to_owned(),
        "json_schema".to_owned(),
        "reasoning_effort".to_owned(),
        "streaming".to_owned(),
    ]];
    for r in reports {
        rows.push([
            role_name(r.role).to_owned(),
            r.model.clone(),
            cell(&r.exists).to_owned(),
            cell(&r.chat).to_owned(),
            cell(&r.tool_choice).to_owned(),
            cell(&r.json_schema).to_owned(),
            format!("{} ({})", cell(&r.reasoning_effort), r.effort),
            cell(&r.streaming).to_owned(),
        ]);
    }
    let widths: Vec<usize> = (0..rows[0].len())
        .map(|i| {
            rows.iter()
                .map(|row| row[i].chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();

    let mut text = String::new();
    for row in &rows {
        let line: Vec<String> = row
            .iter()
            .zip(&widths)
            .map(|(value, &width)| format!("{value:width$}"))
            .collect();
        text.push_str(line.join("  ").trim_end());
        text.push('\n');
    }
    for r in reports {
        for (item, check) in [
            ("id", &r.exists),
            ("chat", &r.chat),
            ("tool_choice: required", &r.tool_choice),
            ("json_schema", &r.json_schema),
            ("reasoning_effort", &r.reasoning_effort),
            ("streaming", &r.streaming),
        ] {
            if let Check::Fail(reason) = check {
                text.push_str(&format!("{} {item}: {reason}\n", role_name(r.role)));
            }
        }
    }
    text
}

/// `path` の `[capabilities]` を `capabilities` に書き換える。ほかの節・キー・コメントはそのまま残す。
///
/// 書き込む前にファイル全体が `config.toml` として読めることと、`[api]`・`[models]`・
/// `[reasoning_effort]` が確かめたとき（`checked`）と同じであることを確かめ、どちらかが崩れていれば何も変えない。
/// 確かめている間に利用者が接続先やモデルを書き換えると、別のモデルの結果を記録してしまうため。
/// 書き込みは同じディレクトリの一時ファイルに書いてから rename するので、途中で失敗しても
/// 元のファイルは壊れない。内容が変わらなければ書き込まない。
fn write_capabilities(path: &Path, checked: &Config, capabilities: &Capabilities) -> Result<()> {
    let path_error = |source| Error::Io {
        path: path.to_owned(),
        source,
    };
    // config.toml がリンクなら、リンクを一時ファイルで置き換えないよう、指す先に書く。
    // 指す先が無いリンクは、置き換えてしまわないよう止める
    let target = match fs::canonicalize(path) {
        Ok(target) => target,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            if fs::symlink_metadata(path).is_ok() {
                return Err(path_error(io::Error::new(
                    io::ErrorKind::NotFound,
                    "config.toml is a symbolic link to a missing file",
                )));
            }
            path.to_owned()
        }
        Err(source) => return Err(path_error(source)),
    };
    let io_error = |source| Error::Io {
        path: target.clone(),
        source,
    };
    let original = match fs::read_to_string(&target) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(source) => return Err(io_error(source)),
    };
    let text = original.as_deref().unwrap_or("");
    let current = toml::from_str::<Config>(text).map_err(|source| Error::ConfigParse {
        path: target.clone(),
        source,
    })?;
    if (&current.api, &current.models, &current.reasoning_effort)
        != (&checked.api, &checked.models, &checked.reasoning_effort)
    {
        return Err(io_error(io::Error::other(
            "[api], [models] or [reasoning_effort] changed while doctor was running; run doctor again",
        )));
    }
    let mut document: DocumentMut = text.parse().map_err(|e: toml_edit::TomlError| {
        io_error(io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
    })?;
    set_capabilities(&mut document, capabilities).ok_or_else(|| {
        io_error(io::Error::new(
            io::ErrorKind::InvalidData,
            "[capabilities] is not a table",
        ))
    })?;
    let updated = document.to_string();
    if original.as_deref() == Some(updated.as_str()) {
        return Ok(());
    }

    if let Some(dir) = target.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        fs::create_dir_all(dir).map_err(io_error)?;
    }
    let file_name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temporary = target.with_file_name(format!(".{file_name}.{}.tmp", std::process::id()));
    // 同じ名前のファイルが既にあれば、自分のものではないので消さずに止める
    let mut file = fs::File::create_new(&temporary).map_err(io_error)?;
    let written = (|| {
        file.write_all(updated.as_bytes())?;
        if original.is_some() {
            // API キーを書かないファイルでも、利用者が絞った権限は保つ
            file.set_permissions(fs::metadata(&target)?.permissions())?;
        }
        file.sync_all()?;
        fs::rename(&temporary, &target)
    })();
    if let Err(source) = written {
        let _ = fs::remove_file(&temporary);
        return Err(io_error(source));
    }
    Ok(())
}

/// `document` の `[capabilities.<役割>]` の 3 キーを書き換える。無い節・キーは足す。
/// 既にある値は前後の空白とコメントを残して値だけを替える。
/// `[capabilities]` か役割が表でなければ `None`（`Config` として読めた後なので起きない）。
fn set_capabilities(document: &mut DocumentMut, capabilities: &Capabilities) -> Option<()> {
    let section = document
        .as_table_mut()
        .entry("capabilities")
        .or_insert_with(|| {
            // 中身の無い [capabilities] の見出しを出さず、[capabilities.light] などだけを書く
            let mut table = Table::new();
            table.set_implicit(true);
            Item::Table(table)
        });
    let inline = section.is_inline_table();
    let section = section.as_table_like_mut()?;
    for (name, role) in [
        ("light", &capabilities.light),
        ("generate", &capabilities.generate),
        ("reason", &capabilities.reason),
    ] {
        let table = section
            .entry(name)
            .or_insert(if inline {
                Item::Value(InlineTable::new().into())
            } else {
                Item::Table(Table::new())
            })
            .as_table_like_mut()?;
        set_bool(table, "tool_choice_required", role.tool_choice_required);
        set_bool(table, "json_schema", role.json_schema);
        set_bool(table, "reasoning_effort", role.reasoning_effort);
        set_bool(table, "streaming", role.streaming);
    }
    Some(())
}

fn set_bool(table: &mut dyn TableLike, key: &str, value: bool) {
    match table.get_mut(key).and_then(Item::as_value_mut) {
        Some(existing) => {
            let decor = existing.decor().clone();
            *existing = value.into();
            *existing.decor_mut() = decor;
        }
        None => {
            table.insert(key, toml_edit::value(value));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{FakeChatClient, ToolCall};

    /// 既定値を書いた config.toml に、利用者のコメントとほかの節を混ぜたもの。
    const CONFIG: &str = r#"# my settings
[api]
base_url = "http://localhost:8000/v1" # local
api_key_env = "MOLT_DOCTOR_TEST_KEY"
timeout_secs = 600

[models]
light = "light-model"
generate = "generate-model"
reason = "reason-model"

[capabilities.light]
tool_choice_required = false # measured by doctor
json_schema = false
reasoning_effort = false
streaming = false

[capabilities.generate]
tool_choice_required = false
json_schema = false
reasoning_effort = false
streaming = false

[capabilities.reason]
tool_choice_required = false
json_schema = false
reasoning_effort = false
streaming = false
"#;

    const MODELS: [&str; 3] = ["light-model", "generate-model", "reason-model"];

    struct Env {
        _dir: tempfile::TempDir,
        path: PathBuf,
    }

    fn env_with(text: Option<&str>) -> Env {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        if let Some(text) = text {
            fs::write(&path, text).unwrap();
        }
        Env { _dir: dir, path }
    }

    fn reply(text: &str) -> ChatResponse {
        ChatResponse {
            content: Some(text.to_owned()),
            tool_calls: Vec::new(),
        }
    }

    fn tool_call(name: &str, arguments: &str) -> ChatResponse {
        ChatResponse {
            content: None,
            tool_calls: vec![ToolCall {
                id: "call_0".to_owned(),
                name: name.to_owned(),
                arguments: arguments.to_owned(),
            }],
        }
    }

    /// 1 つの役割の 6 回の chat（素の chat・tool_choice・json_schema・reasoning_effort・流した
    /// 文章・流したツールの呼び出し）への応答。
    struct Replies {
        chat: std::result::Result<ChatResponse, ChatError>,
        tool_choice: std::result::Result<ChatResponse, ChatError>,
        json_schema: std::result::Result<ChatResponse, ChatError>,
        reasoning_effort: std::result::Result<ChatResponse, ChatError>,
        streamed_text: std::result::Result<ChatResponse, ChatError>,
        streamed_tool: std::result::Result<ChatResponse, ChatError>,
    }

    fn passing() -> Replies {
        Replies {
            chat: Ok(reply("OK")),
            tool_choice: Ok(tool_call(TOOL_NAME, r#"{"answer": 5}"#)),
            json_schema: Ok(reply(r#"{"answer": 5}"#)),
            reasoning_effort: Ok(reply("OK")),
            streamed_text: Ok(reply("OK")),
            streamed_tool: Ok(tool_call(TOOL_NAME, r#"{"answer": 5}"#)),
        }
    }

    fn push(fake: &FakeChatClient, replies: Replies) {
        fake.push_chat(replies.chat);
        fake.push_chat(replies.tool_choice);
        fake.push_chat(replies.json_schema);
        fake.push_chat(replies.reasoning_effort);
        fake.push_chat(replies.streamed_text);
        fake.push_chat(replies.streamed_tool);
    }

    fn fake_with(roles: [Replies; 3]) -> FakeChatClient {
        let fake = FakeChatClient::new();
        fake.push_models(Ok(MODELS.iter().map(|&m| m.to_owned()).collect()));
        for replies in roles {
            push(&fake, replies);
        }
        fake
    }

    fn status(status: u16) -> ChatError {
        ChatError::Status {
            status,
            body: "error".to_owned(),
        }
    }

    /// `env` の config.toml を読み、`client` に対して doctor を走らせ、成否と出力を返す。
    fn run_doctor(env: &Env, client: &dyn ChatClient) -> (bool, String) {
        let config = Config::load(&env.path).unwrap();
        let mut out = Vec::new();
        let ok = run(&env.path, &config, client, &mut out).unwrap();
        (ok, String::from_utf8(out).unwrap())
    }

    fn written(env: &Env) -> Config {
        Config::load(&env.path).unwrap()
    }

    fn all(value: bool) -> RoleCapabilities {
        RoleCapabilities {
            tool_choice_required: value,
            json_schema: value,
            reasoning_effort: value,
            streaming: value,
        }
    }

    // --- 接続の失敗 ---

    #[test]
    fn doctor_reports_a_connection_failure_when_models_fails_and_leaves_config_untouched() {
        let original = CONFIG.replace("json_schema = false", "json_schema = true");
        let env = env_with(Some(&original));
        let fake = FakeChatClient::new();
        fake.push_models(Err(status(503)));

        let (ok, output) = run_doctor(&env, &fake);

        assert!(!ok);
        assert!(output.contains("connection failed"), "{output}");
        assert_eq!(fs::read_to_string(&env.path).unwrap(), original);
        assert!(fake.requests().is_empty());
    }

    #[test]
    fn doctor_treats_any_models_error_as_a_connection_failure() {
        for error in [
            ChatError::FakeExhausted,
            ChatError::InvalidResponse("not json".to_owned()),
            status(404),
        ] {
            let env = env_with(Some(CONFIG));
            let fake = FakeChatClient::new();
            fake.push_models(Err(error));

            let (ok, output) = run_doctor(&env, &fake);

            assert!(!ok, "{output}");
            assert!(output.contains("connection failed"), "{output}");
            assert_eq!(fs::read_to_string(&env.path).unwrap(), CONFIG);
        }
    }

    #[test]
    fn doctor_does_not_create_config_toml_on_a_connection_failure() {
        let env = env_with(None);
        let fake = FakeChatClient::new();
        fake.push_models(Err(status(503)));

        let (ok, _) = run_doctor(&env, &fake);

        assert!(!ok);
        assert!(!env.path.exists());
    }

    #[test]
    fn doctor_reports_an_api_key_over_plain_http_as_a_configuration_error_and_leaves_config_untouched()
     {
        let env = env_with(Some(CONFIG));
        let fake = FakeChatClient::new();
        fake.push_models(Err(ChatError::ApiKeyOverPlainHttp {
            env: "MOLT_DOCTOR_TEST_KEY".to_owned(),
            host: "gpu-box.lan:80".to_owned(),
        }));

        let (ok, output) = run_doctor(&env, &fake);

        assert!(!ok);
        assert!(output.starts_with("configuration error: "), "{output}");
        assert!(!output.contains("connection failed"), "{output}");
        assert_eq!(fs::read_to_string(&env.path).unwrap(), CONFIG);
    }

    #[test]
    fn doctor_reports_a_connection_failure_during_a_chat_and_leaves_config_untouched() {
        for code in [401, 403, 408, 429, 500, 503] {
            let env = env_with(Some(CONFIG));
            // light はすべて通り、generate の素の chat で接続が切れる
            let fake = fake_with([
                passing(),
                Replies {
                    chat: Err(status(code)),
                    ..passing()
                },
                passing(),
            ]);

            let (ok, output) = run_doctor(&env, &fake);

            assert!(!ok, "{code}: {output}");
            assert!(
                output.contains("connection failed while checking generate"),
                "{code}: {output}"
            );
            assert_eq!(fs::read_to_string(&env.path).unwrap(), CONFIG, "{code}");
        }
    }

    #[test]
    fn doctor_reports_a_timeout_during_a_chat_as_a_connection_failure_and_leaves_config_untouched()
    {
        let env = env_with(Some(CONFIG));
        let fake = fake_with([
            passing(),
            Replies {
                chat: Err(ChatError::TimedOut { secs: 300 }),
                ..passing()
            },
            passing(),
        ]);

        let (ok, output) = run_doctor(&env, &fake);

        assert!(!ok, "{output}");
        assert!(
            output.contains("connection failed while checking generate"),
            "{output}"
        );
        assert!(
            output.contains("did not respond within 300 seconds"),
            "{output}"
        );
        assert_eq!(fs::read_to_string(&env.path).unwrap(), CONFIG);
    }

    // --- 5 項目の判定 ---

    #[test]
    fn doctor_writes_every_capability_when_every_check_passes() {
        let env = env_with(Some(CONFIG));
        let fake = fake_with([passing(), passing(), passing()]);

        let (ok, output) = run_doctor(&env, &fake);

        assert!(ok, "{output}");
        let capabilities = written(&env).capabilities;
        assert_eq!(capabilities.light, all(true));
        assert_eq!(capabilities.generate, all(true));
        assert_eq!(capabilities.reason, all(true));
    }

    #[test]
    fn doctor_marks_a_model_missing_from_models_and_skips_its_chats() {
        let env = env_with(Some(CONFIG));
        let fake = FakeChatClient::new();
        // light-model だけが一覧に無い
        fake.push_models(Ok(vec![
            "generate-model".to_owned(),
            "reason-model".to_owned(),
        ]));
        push(&fake, passing());
        push(&fake, passing());

        let (ok, output) = run_doctor(&env, &fake);

        assert!(ok, "{output}");
        assert!(
            output.contains("light id: not in the endpoint's model list"),
            "{output}"
        );
        assert!(
            fake.requests().iter().all(|r| r.model != "light-model"),
            "{:?}",
            fake.requests()
        );
        let capabilities = written(&env).capabilities;
        assert_eq!(capabilities.light, all(false));
        assert_eq!(capabilities.generate, all(true));
    }

    #[test]
    fn doctor_reports_a_plain_chat_without_content_as_failed() {
        let env = env_with(Some(CONFIG));
        let fake = fake_with([
            Replies {
                chat: Ok(ChatResponse {
                    content: Some("  ".to_owned()),
                    tool_calls: Vec::new(),
                }),
                ..passing()
            },
            passing(),
            Replies {
                chat: Err(status(400)),
                ..passing()
            },
        ]);

        let (ok, output) = run_doctor(&env, &fake);

        assert!(ok, "{output}");
        assert!(
            output.contains("light chat: the reply had no content"),
            "{output}"
        );
        assert!(output.contains("reason chat: "), "{output}");
        assert!(!output.contains("generate chat"), "{output}");
    }

    #[test]
    fn doctor_sets_tool_choice_required_false_when_the_reply_has_no_tool_call() {
        let env = env_with(Some(CONFIG));
        let fake = fake_with([
            Replies {
                tool_choice: Ok(reply(r#"{"answer": 5}"#)),
                ..passing()
            },
            passing(),
            passing(),
        ]);

        run_doctor(&env, &fake);

        let capabilities = written(&env).capabilities;
        assert!(!capabilities.light.tool_choice_required);
        assert!(capabilities.light.json_schema && capabilities.light.reasoning_effort);
        assert!(capabilities.generate.tool_choice_required);
    }

    #[test]
    fn doctor_sets_tool_choice_required_false_for_a_call_to_an_unknown_tool() {
        let env = env_with(Some(CONFIG));
        let fake = fake_with([
            Replies {
                tool_choice: Ok(tool_call("other_tool", r#"{"answer": 5}"#)),
                ..passing()
            },
            passing(),
            passing(),
        ]);

        run_doctor(&env, &fake);

        assert!(!written(&env).capabilities.light.tool_choice_required);
    }

    #[test]
    fn doctor_sets_tool_choice_required_false_for_unreadable_arguments() {
        let env = env_with(Some(CONFIG));
        let fake = fake_with([
            Replies {
                tool_choice: Ok(tool_call(TOOL_NAME, r#"{"answer": "#)),
                ..passing()
            },
            passing(),
            passing(),
        ]);

        run_doctor(&env, &fake);

        assert!(!written(&env).capabilities.light.tool_choice_required);
    }

    #[test]
    fn doctor_sets_tool_choice_required_false_when_the_request_is_rejected() {
        let env = env_with(Some(CONFIG));
        let fake = fake_with([
            Replies {
                tool_choice: Err(status(400)),
                ..passing()
            },
            passing(),
            passing(),
        ]);

        let (ok, _) = run_doctor(&env, &fake);

        assert!(ok);
        assert!(!written(&env).capabilities.light.tool_choice_required);
    }

    #[test]
    fn doctor_sets_json_schema_false_when_the_reply_does_not_follow_the_schema() {
        for bad in [
            r#"{"answer": "five"}"#,
            r#"{"answer": 5, "extra": true}"#,
            r#"{"result": 5}"#,
            "The answer is 5.",
        ] {
            let env = env_with(Some(CONFIG));
            let fake = fake_with([
                passing(),
                Replies {
                    json_schema: Ok(reply(bad)),
                    ..passing()
                },
                passing(),
            ]);

            let (ok, output) = run_doctor(&env, &fake);

            assert!(ok, "{output}");
            let capabilities = written(&env).capabilities;
            assert!(!capabilities.generate.json_schema, "{bad}");
            assert!(capabilities.generate.tool_choice_required, "{bad}");
            assert!(capabilities.light.json_schema, "{bad}");
        }
    }

    #[test]
    fn doctor_sets_reasoning_effort_false_when_the_request_is_rejected() {
        let env = env_with(Some(CONFIG));
        let fake = fake_with([
            passing(),
            passing(),
            Replies {
                reasoning_effort: Err(status(400)),
                ..passing()
            },
        ]);

        let (ok, output) = run_doctor(&env, &fake);

        assert!(ok, "{output}");
        let capabilities = written(&env).capabilities;
        assert!(!capabilities.reason.reasoning_effort);
        assert!(capabilities.reason.json_schema);
        assert!(capabilities.light.reasoning_effort);
        assert!(output.contains("reason reasoning_effort: "), "{output}");
    }

    #[test]
    fn doctor_sets_streaming_false_when_a_streamed_reply_has_no_tool_call_or_fails() {
        let env = env_with(Some(CONFIG));
        let no_tool = Replies {
            streamed_tool: Ok(reply("OK")),
            ..passing()
        };
        let broken = Replies {
            streamed_text: Err(ChatError::InvalidResponse(
                "the stream ended before its finish_reason".to_owned(),
            )),
            ..passing()
        };
        let fake = fake_with([passing(), no_tool, broken]);

        let (_, output) = run_doctor(&env, &fake);

        let capabilities = written(&env).capabilities;
        assert!(capabilities.light.streaming);
        assert!(!capabilities.generate.streaming);
        assert!(!capabilities.reason.streaming);
        assert!(
            output.contains("generate streaming: the reply had no tool call"),
            "{output}"
        );
        assert!(output.contains("reason streaming: "), "{output}");
    }

    #[test]
    fn doctor_sends_each_feature_only_in_its_own_check() {
        // 既存の [capabilities] が真でも、素の chat に reasoning_effort を混ぜない
        let env = env_with(Some(&CONFIG.replace("= false", "= true")));
        let fake = fake_with([passing(), passing(), passing()]);

        run_doctor(&env, &fake);

        let requests = fake.requests();
        assert_eq!(requests.len(), 18);
        let efforts = ["low", "medium", "high"];
        for (i, role) in requests.chunks(6).enumerate() {
            assert!(role.iter().all(|r| r.model == MODELS[i]));
            let [chat, tool, json, effort, streamed_text, streamed_tool] = role else {
                unreachable!()
            };
            assert!(
                [chat, tool, json, effort].iter().all(|r| !r.stream),
                "only the streaming check streams"
            );
            assert!(streamed_text.stream && streamed_text.tools.is_empty());
            assert!(streamed_tool.stream);
            assert_eq!(streamed_tool.tool_choice, Some(ToolChoice::Required));
            assert_eq!(
                (&chat.tools.len(), &chat.tool_choice, &chat.response_format),
                (&0, &None, &None)
            );
            assert_eq!(chat.reasoning_effort, None);
            assert_eq!(tool.tool_choice, Some(ToolChoice::Required));
            assert_eq!(tool.tools.len(), 1);
            assert_eq!(tool.reasoning_effort, None);
            assert!(matches!(
                json.response_format,
                Some(ResponseFormat::JsonSchema { .. })
            ));
            assert_eq!(
                (json.tools.len(), json.reasoning_effort.as_deref()),
                (0, None)
            );
            assert_eq!(effort.reasoning_effort.as_deref(), Some(efforts[i]));
            assert_eq!((effort.tools.len(), &effort.response_format), (0, &None));
        }
    }

    #[test]
    fn doctor_prints_a_row_per_role_with_the_results() {
        let env = env_with(Some(CONFIG));
        let fake = fake_with([
            passing(),
            Replies {
                json_schema: Ok(reply("no")),
                ..passing()
            },
            passing(),
        ]);

        let (_, output) = run_doctor(&env, &fake);

        let row = |name: &str| {
            output
                .lines()
                .find(|line| line.starts_with(name))
                .unwrap_or_else(|| panic!("no row for {name}: {output}"))
                .split_whitespace()
                .collect::<Vec<_>>()
        };
        assert_eq!(
            row("light"),
            [
                "light",
                "light-model",
                "ok",
                "ok",
                "ok",
                "ok",
                "ok",
                "(low)",
                "ok"
            ]
        );
        assert_eq!(
            row("generate"),
            [
                "generate",
                "generate-model",
                "ok",
                "ok",
                "ok",
                "no",
                "ok",
                "(medium)",
                "ok"
            ]
        );
    }

    // --- config.toml の書き戻し ---

    #[test]
    fn doctor_changes_only_the_capability_values_and_keeps_everything_else() {
        let env = env_with(Some(CONFIG));
        let fake = fake_with([passing(), passing(), passing()]);

        run_doctor(&env, &fake);

        let expected = CONFIG
            .replace(
                "tool_choice_required = false",
                "tool_choice_required = true",
            )
            .replace("json_schema = false", "json_schema = true")
            .replace("reasoning_effort = false", "reasoning_effort = true")
            .replace("streaming = false", "streaming = true");
        assert_eq!(fs::read_to_string(&env.path).unwrap(), expected);
    }

    #[test]
    fn doctor_adds_capabilities_to_a_config_without_them() {
        let without = CONFIG.split("[capabilities.light]").next().unwrap();
        let env = env_with(Some(without));
        let fake = fake_with([passing(), passing(), passing()]);

        run_doctor(&env, &fake);

        let text = fs::read_to_string(&env.path).unwrap();
        assert!(text.starts_with(without), "{text}");
        assert!(!text.contains("[capabilities]\n"), "{text}");
        let config = written(&env);
        assert_eq!(config.capabilities.reason, all(true));
        assert_eq!(config.models.light, "light-model");
    }

    #[test]
    fn doctor_creates_config_toml_with_only_capabilities_when_it_is_missing() {
        let env = env_with(None);
        // ファイルが無いので既定のモデル ID で確かめる
        let fake = FakeChatClient::new();
        let defaults = Config::default().models;
        fake.push_models(Ok(vec![defaults.light, defaults.generate, defaults.reason]));
        for _ in 0..3 {
            push(&fake, passing());
        }

        let (ok, output) = run_doctor(&env, &fake);

        assert!(ok, "{output}");
        let config = written(&env);
        assert_eq!(config.capabilities.light, all(true));
        assert_eq!(
            Config {
                capabilities: Capabilities::default(),
                ..config
            },
            Config::default()
        );
    }

    #[test]
    fn doctor_updates_an_inline_capabilities_table() {
        // 節の見出しより後に書くとその節のキーになるので、先頭に置く
        let inline = format!(
            "capabilities = {{ light = {{ json_schema = false }} }}\n{}",
            CONFIG.split("[capabilities.light]").next().unwrap()
        );
        let env = env_with(Some(&inline));
        let fake = fake_with([passing(), passing(), passing()]);

        let (ok, output) = run_doctor(&env, &fake);

        assert!(ok, "{output}");
        let config = written(&env);
        assert_eq!(config.capabilities.light, all(true));
        assert_eq!(config.capabilities.generate, all(true));
    }

    #[test]
    fn doctor_run_twice_with_the_same_results_leaves_the_same_file() {
        let env = env_with(Some(CONFIG));
        run_doctor(&env, &fake_with([passing(), passing(), passing()]));
        let first = fs::read_to_string(&env.path).unwrap();

        run_doctor(&env, &fake_with([passing(), passing(), passing()]));

        assert_eq!(fs::read_to_string(&env.path).unwrap(), first);
    }

    #[test]
    fn doctor_rejects_an_unreadable_config_without_writing() {
        let broken = format!("{CONFIG}\n[unknown]\nkey = 1\n");
        let env = env_with(Some(&broken));
        let fake = fake_with([passing(), passing(), passing()]);
        let config = Config::default();
        let mut out = Vec::new();

        let result = run(&env.path, &config, &fake, &mut out);

        assert!(
            matches!(result, Err(Error::ConfigParse { .. })),
            "{result:?}"
        );
        assert_eq!(fs::read_to_string(&env.path).unwrap(), broken);
    }

    #[test]
    fn doctor_does_not_write_when_api_models_or_reasoning_effort_changed_while_checking() {
        let edits: [fn(&mut Config); 3] = [
            |c| c.api.base_url = "http://another-endpoint/v1".to_owned(),
            |c| c.models.reason = "another-model".to_owned(),
            |c| c.reasoning_effort.light = "medium".to_owned(),
        ];
        for edit in edits {
            let env = env_with(Some(CONFIG));
            // 確かめたときの設定と、書き戻すときのファイルの設定が違う
            let mut checked = Config::load(&env.path).unwrap();
            edit(&mut checked);
            let fake = FakeChatClient::new();
            fake.push_models(Ok(vec![
                checked.models.light.clone(),
                checked.models.generate.clone(),
                checked.models.reason.clone(),
            ]));
            for _ in 0..3 {
                push(&fake, passing());
            }
            let mut out = Vec::new();

            let result = run(&env.path, &checked, &fake, &mut out);

            assert!(matches!(result, Err(Error::Io { .. })), "{result:?}");
            assert_eq!(fs::read_to_string(&env.path).unwrap(), CONFIG);
        }
    }

    #[cfg(unix)]
    #[test]
    fn doctor_stops_at_a_symlinked_config_toml_whose_target_is_missing() {
        let env = env_with(None);
        let missing = env.path.with_file_name("missing.toml");
        std::os::unix::fs::symlink(&missing, &env.path).unwrap();
        let fake = fake_with([passing(), passing(), passing()]);
        let mut out = Vec::new();

        let result = run(&env.path, &Config::default(), &fake, &mut out);

        assert!(matches!(result, Err(Error::Io { .. })), "{result:?}");
        assert!(
            fs::symlink_metadata(&env.path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(!missing.exists());
    }

    #[cfg(unix)]
    #[test]
    fn doctor_keeps_the_permissions_of_config_toml() {
        use std::os::unix::fs::PermissionsExt;
        let env = env_with(Some(CONFIG));
        fs::set_permissions(&env.path, fs::Permissions::from_mode(0o600)).unwrap();

        run_doctor(&env, &fake_with([passing(), passing(), passing()]));

        let mode = fs::metadata(&env.path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert!(written(&env).capabilities.light.json_schema);
    }

    #[cfg(unix)]
    #[test]
    fn doctor_writes_through_a_symlinked_config_toml_and_keeps_the_link() {
        let env = env_with(None);
        let real = env.path.with_file_name("real.toml");
        fs::write(&real, CONFIG).unwrap();
        std::os::unix::fs::symlink(&real, &env.path).unwrap();

        run_doctor(&env, &fake_with([passing(), passing(), passing()]));

        assert!(
            fs::symlink_metadata(&env.path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(
            fs::read_to_string(&real)
                .unwrap()
                .contains("json_schema = true")
        );
    }

    #[cfg(unix)]
    #[test]
    fn doctor_does_not_rewrite_config_toml_when_nothing_changes() {
        use std::os::unix::fs::MetadataExt;
        let env = env_with(Some(CONFIG));
        run_doctor(&env, &fake_with([passing(), passing(), passing()]));
        let first = fs::metadata(&env.path).unwrap().ino();

        run_doctor(&env, &fake_with([passing(), passing(), passing()]));

        // rename で置き換えていれば inode が変わる
        assert_eq!(fs::metadata(&env.path).unwrap().ino(), first);
    }
}
