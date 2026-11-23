//! okfast — OKF 0.2 を最高速で扱うためのライブラリ。
//!
//! - [`bundle`] — バンドルの読み込み（frontmatter は未知のキーごと保持）
//! - [`config`] — バンドルごとの設定 `okfast.toml`
//! - [`aliases`] — 設定に従って同義語の辞書を作る
//! - [`search`] — 同義語で言い換える行単位の検索
//! - [`daemon`] — すべてをメモリに持ち、ファイルの変更を監視して答える常駐プロセス
//! - [`mcp`] — AI のクライアントからツールとして呼ばれる MCP サーバー

pub mod aliases;
pub mod bundle;
pub mod cache;
pub mod check;
pub mod command;
pub mod config;
pub mod daemon;
pub mod error;
pub mod frontmatter;
pub mod init;
pub mod markdown;
pub mod mcp;
pub mod normalize;
pub mod search;
pub mod selector;
pub mod textindex;

pub use error::{Error, Result};

/// 対応する OKF 仕様のバージョン。
pub const OKF_VERSION: &str = "0.2";
