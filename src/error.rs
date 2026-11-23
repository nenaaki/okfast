//! okfast のエラー型。

use std::fmt;
use std::path::PathBuf;

/// okfast の処理で起こるエラー。
#[derive(Debug)]
pub enum Error {
    /// ファイル・ディレクトリの読み込みに失敗した。
    Io {
        /// 対象のパス。
        path: PathBuf,
        /// 元のエラー。
        source: std::io::Error,
    },
    /// 設定ファイル（okfast.toml）が不正。
    Config {
        /// 設定ファイルのパス（ない場合は既定値）。
        path: Option<PathBuf>,
        /// 内容。
        message: String,
    },
    /// glob パターンが不正。
    Glob {
        /// 問題のパターン。
        pattern: String,
        /// 内容。
        message: String,
    },
    /// 検索の指定が不正。
    Query(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Error::Config {
                path: Some(p),
                message,
            } => write!(f, "設定ファイル {}: {message}", p.display()),
            Error::Config {
                path: None,
                message,
            } => write!(f, "設定: {message}"),
            Error::Glob { pattern, message } => write!(f, "glob `{pattern}`: {message}"),
            Error::Query(m) => write!(f, "検索の指定: {m}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// okfast の Result。
pub type Result<T> = std::result::Result<T, Error>;
