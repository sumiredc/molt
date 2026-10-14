//! `config.toml` の構造体と既定値、molt の置き場所の解決。
//!
//! 既定値の正はこのファイルのコード（PRD §8.7）。`examples/config.toml` は同じ値を写した例。

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{Error, Result};

/// molt の置き場所の中にある設定ファイルの名前。
const CONFIG_FILE_NAME: &str = "config.toml";

/// `config.toml` の全体。書かれていない節・キーは既定値になる。
/// 知らないキーは綴りの誤りとみなして読み込みを失敗させる（誤ったキーが黙って既定値に化けないように）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub api: Api,
    pub models: Models,
    pub reasoning_effort: ReasoningEffort,
    pub capabilities: Capabilities,
}

/// `[api]`: 接続先の OpenAI 互換エンドポイント。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Api {
    /// `/chat/completions` の手前までの URL。
    pub base_url: String,
    /// API キーを読む環境変数の名前。キーそのものは設定ファイルに書かない（PRD §3）。
    /// キーの要らないエンドポイント（ローカルのモデルなど）では空にする。空なら Authorization を送らない。
    pub api_key_env: String,
    /// 1 リクエストのタイムアウト（秒）。
    pub timeout_secs: u64,
}

impl Default for Api {
    fn default() -> Self {
        Self {
            base_url: "https://api.tokenfactory.nebius.com/v1".to_owned(),
            api_key_env: "NEBIUS_API_KEY".to_owned(),
            // ローカルのモデルは prefill が遅いため長めにとる
            timeout_secs: 300,
        }
    }
}

/// `[models]`: 役割ごとのモデル ID（PRD §8.7）。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Models {
    pub light: String,
    pub generate: String,
    pub reason: String,
}

impl Default for Models {
    fn default() -> Self {
        Self {
            light: "nvidia/Nemotron-3_5-Lightning".to_owned(),
            generate: "nvidia/nemotron-3-super-120b-a12b".to_owned(),
            reason: "nvidia/Nemotron-3-Ultra-550b-a55b".to_owned(),
        }
    }
}

/// `[reasoning_effort]`: 役割ごとの `reasoning_effort`。
/// 送るのは `[capabilities]` でその役割が受け付けると分かっているときだけ（PRD §11）。
/// 受け付ける値はエンドポイントとモデルで違うので、値の検査はせず、そのまま送る。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ReasoningEffort {
    pub light: String,
    pub generate: String,
    pub reason: String,
}

impl Default for ReasoningEffort {
    fn default() -> Self {
        Self {
            light: "low".to_owned(),
            generate: "medium".to_owned(),
            reason: "high".to_owned(),
        }
    }
}

/// `[capabilities]`: 役割ごとに、モデルが受け付けると `molt doctor` が確かめた機能。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Capabilities {
    pub light: RoleCapabilities,
    pub generate: RoleCapabilities,
    pub reason: RoleCapabilities,
}

/// 1 つの役割の機能。確かめるまでは使えないものとして扱うので、既定はすべて偽。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RoleCapabilities {
    pub tool_choice_required: bool,
    pub json_schema: bool,
    pub reasoning_effort: bool,
    /// 応答を server-sent events で流して受けられる（PRD §8.7）。
    pub streaming: bool,
}

impl Config {
    /// `path` の `config.toml` を読む。ファイルが無ければ既定値を返す。
    pub fn load(path: &Path) -> Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(source) => {
                return Err(Error::Io {
                    path: path.to_owned(),
                    source,
                });
            }
        };
        toml::from_str(&text).map_err(|source| Error::ConfigParse {
            path: path.to_owned(),
            source,
        })
    }
}

/// molt の置き場所（プロファイルと `config.toml` を置くディレクトリ）を決める。
///
/// 優先順位は `$MOLT_HOME` → `$XDG_CONFIG_HOME/molt` → `$HOME/.config/molt`（PRD §8.1）。
/// 環境変数は `var` で読む。空の値は未設定と同じに扱い、`$XDG_CONFIG_HOME` は
/// XDG Base Directory の決まりに従って相対パスなら無視する。
pub fn molt_home(var: impl Fn(&str) -> Option<OsString>) -> Result<PathBuf> {
    let get = |name: &str| {
        var(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    if let Some(dir) = get("MOLT_HOME") {
        return Ok(dir);
    }
    if let Some(dir) = get("XDG_CONFIG_HOME").filter(|dir| dir.is_absolute()) {
        return Ok(dir.join("molt"));
    }
    if let Some(home) = get("HOME") {
        return Ok(home.join(".config").join("molt"));
    }
    Err(Error::NoHomeDir)
}

/// molt の置き場所 `home` の中の `config.toml` のパス。
pub fn config_path(home: &Path) -> PathBuf {
    home.join(CONFIG_FILE_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |name| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(value))
        }
    }

    const ALL: &[(&str, &str)] = &[
        ("MOLT_HOME", "/molt-home"),
        ("XDG_CONFIG_HOME", "/xdg"),
        ("HOME", "/home/user"),
    ];

    #[test]
    fn molt_home_prefers_molt_home_over_xdg_and_home() {
        assert_eq!(molt_home(env(ALL)).unwrap(), PathBuf::from("/molt-home"));
    }

    #[test]
    fn molt_home_uses_xdg_config_home_when_molt_home_is_unset() {
        let vars = [("XDG_CONFIG_HOME", "/xdg"), ("HOME", "/home/user")];
        assert_eq!(molt_home(env(&vars)).unwrap(), PathBuf::from("/xdg/molt"));
    }

    #[test]
    fn molt_home_falls_back_to_home_dot_config() {
        let vars = [("HOME", "/home/user")];
        assert_eq!(
            molt_home(env(&vars)).unwrap(),
            PathBuf::from("/home/user/.config/molt")
        );
    }

    #[test]
    fn molt_home_treats_empty_values_as_unset() {
        let vars = [
            ("MOLT_HOME", ""),
            ("XDG_CONFIG_HOME", ""),
            ("HOME", "/home/user"),
        ];
        assert_eq!(
            molt_home(env(&vars)).unwrap(),
            PathBuf::from("/home/user/.config/molt")
        );
    }

    #[test]
    fn molt_home_ignores_relative_xdg_config_home() {
        let vars = [("XDG_CONFIG_HOME", "relative"), ("HOME", "/home/user")];
        assert_eq!(
            molt_home(env(&vars)).unwrap(),
            PathBuf::from("/home/user/.config/molt")
        );
    }

    #[test]
    fn molt_home_fails_when_nothing_is_set() {
        assert!(matches!(molt_home(env(&[])), Err(Error::NoHomeDir)));
    }

    #[test]
    fn config_path_is_config_toml_in_molt_home() {
        assert_eq!(
            config_path(Path::new("/molt-home")),
            PathBuf::from("/molt-home/config.toml")
        );
    }

    #[test]
    fn default_config_targets_nebius_with_nemotron_roles() {
        let config = Config::default();
        assert_eq!(
            config.api.base_url,
            "https://api.tokenfactory.nebius.com/v1"
        );
        assert_eq!(config.api.api_key_env, "NEBIUS_API_KEY");
        assert_eq!(config.api.timeout_secs, 300);
        assert_eq!(config.models.light, "nvidia/Nemotron-3_5-Lightning");
        assert_eq!(config.models.generate, "nvidia/nemotron-3-super-120b-a12b");
        assert_eq!(config.models.reason, "nvidia/Nemotron-3-Ultra-550b-a55b");
    }

    #[test]
    fn default_reasoning_effort_is_low_medium_high_for_light_generate_reason() {
        let effort = Config::default().reasoning_effort;
        assert_eq!(effort.light, "low");
        assert_eq!(effort.generate, "medium");
        assert_eq!(effort.reason, "high");
    }

    #[test]
    fn default_capabilities_are_all_false_until_doctor_confirms_them() {
        let none = RoleCapabilities {
            tool_choice_required: false,
            json_schema: false,
            reasoning_effort: false,
            streaming: false,
        };
        let capabilities = Config::default().capabilities;
        assert_eq!(capabilities.light, none);
        assert_eq!(capabilities.generate, none);
        assert_eq!(capabilities.reason, none);
    }

    #[test]
    fn load_returns_defaults_when_config_toml_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::load(&dir.path().join(CONFIG_FILE_NAME)).unwrap();
        assert_eq!(config, Config::default());
    }

    #[test]
    fn load_overrides_only_the_values_written_in_config_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_FILE_NAME);
        std::fs::write(
            &path,
            r#"
[api]
base_url = "http://localhost:8000/v1"
timeout_secs = 600

[models]
reason = "some/other-model"

[reasoning_effort]
light = "high"

[capabilities.generate]
json_schema = true
"#,
        )
        .unwrap();

        let mut expected = Config::default();
        expected.api.base_url = "http://localhost:8000/v1".to_owned();
        expected.api.timeout_secs = 600;
        expected.models.reason = "some/other-model".to_owned();
        expected.reasoning_effort.light = "high".to_owned();
        expected.capabilities.generate.json_schema = true;

        assert_eq!(Config::load(&path).unwrap(), expected);
    }

    #[test]
    fn load_rejects_unknown_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_FILE_NAME);
        std::fs::write(&path, "[api]\nbase_ulr = \"https://example.com/v1\"\n").unwrap();
        assert!(matches!(
            Config::load(&path),
            Err(Error::ConfigParse { path: p, .. }) if p == path
        ));
    }

    #[test]
    fn load_reports_io_errors_other_than_not_found() {
        let dir = tempfile::tempdir().unwrap();
        // ディレクトリはファイルとして読めない
        assert!(matches!(Config::load(dir.path()), Err(Error::Io { .. })));
    }

    #[test]
    fn example_config_toml_parses_to_the_defaults() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/config.toml");
        let text = std::fs::read_to_string(path).unwrap();
        assert_eq!(toml::from_str::<Config>(&text).unwrap(), Config::default());
    }
}
