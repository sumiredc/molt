use std::fmt;
use std::io;
use std::path::PathBuf;

/// molt の module が共通で返すエラー。
#[derive(Debug)]
pub enum Error {
    /// `$MOLT_HOME`・`$XDG_CONFIG_HOME`・`$HOME` のどれからも置き場所が決まらない（未設定・空・XDG が相対パス）。
    NoHomeDir,
    /// ファイルの読み書きに失敗した。
    Io { path: PathBuf, source: io::Error },
    /// `config.toml` が TOML として読めない、または節・キー・値の型が合わない。
    ConfigParse {
        path: PathBuf,
        source: toml::de::Error,
    },
}

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NoHomeDir => write!(
                f,
                "cannot locate the molt directory: set MOLT_HOME, XDG_CONFIG_HOME (absolute path), or HOME"
            ),
            Error::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Error::ConfigParse { path, source } => {
                write!(f, "{}: invalid config: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::NoHomeDir => None,
            Error::Io { source, .. } => Some(source),
            Error::ConfigParse { source, .. } => Some(source),
        }
    }
}
