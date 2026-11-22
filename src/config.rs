//! 設定ファイル `okfast.toml`（バンドルの root に置く）。
//!
//! 同義語の取り出し方はバンドルごとに宣言する。okfast 本体は特定のバンドルの約束を持たない。

use crate::error::{Error, Result};
use crate::selector::{OneOrMany, SelectorSpec};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// 設定の書式の説明（docs/config.md）。`okfast config reference` が出す。
pub const REFERENCE: &str = include_str!("../docs/config.md");

/// 設定ファイルの既定の名前。
pub const FILE_NAME: &str = "okfast.toml";

/// 設定全体。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// 同義語。
    #[serde(default)]
    pub aliases: AliasConfig,
    /// 検索。
    #[serde(default)]
    pub search: SearchConfig,
}

/// 同義語の取り出し方。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AliasConfig {
    /// 表から取り出す。
    #[serde(default)]
    pub table: Vec<TableSource>,
    /// frontmatter から取り出す。
    #[serde(default)]
    pub frontmatter: Vec<FrontmatterSource>,
    /// 照合の規則。
    #[serde(default, rename = "match")]
    pub matching: MatchConfig,
    /// 取り出し元のページを信頼度で絞る。
    #[serde(default)]
    pub trust: Option<TrustConfig>,
}

/// 表の 1 行を 1 つの概念として、同じ行の呼び名を同義語にする。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TableSource {
    /// 対象のページ。
    pub pages: SelectorSpec,
    /// 概念名の列（省略時は概念名で引かない）。
    #[serde(default)]
    pub concept: Option<String>,
    /// 呼び名の列（同じものだと言える呼び名）。複数の列を並べると、どの列の語も呼び名にする。
    pub names: OneOrMany,
    /// セルの区切り。
    #[serde(default = "default_split")]
    pub split: Vec<String>,
    /// 呼び名から外す注記の開き括弧（例: `["（", "("]`）。対応する閉じ括弧までを外す。
    #[serde(default)]
    pub strip_notes: Vec<String>,
    /// この列（複数可）のリンク先を、検索結果の上位に出す。
    #[serde(default)]
    pub boost: Option<OneOrMany>,
    /// 特定のページを検索するときだけ使う列。
    #[serde(default)]
    pub conditional: Vec<ConditionalColumn>,
}

impl TableSource {
    /// 呼び名の列。
    pub fn names_columns(&self) -> Vec<String> {
        self.names.to_vec()
    }

    /// boost の列。
    pub fn boost_columns(&self) -> Vec<String> {
        self.boost
            .as_ref()
            .map(OneOrMany::to_vec)
            .unwrap_or_default()
    }

    /// 表を使うのに必要な列（呼び名の列と概念名の列）。
    pub fn required_columns(&self) -> Vec<String> {
        let mut v = self.names_columns();
        v.extend(self.concept.clone());
        v
    }
}

/// 条件付きの列。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConditionalColumn {
    /// 列名。
    pub column: String,
    /// 検索対象のページがこれに当たるときだけ使う。
    pub when: SelectorSpec,
}

/// ページの title と、frontmatter の指定キーの値を同義語にする。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrontmatterSource {
    /// 対象のページ（省略時はすべて）。
    #[serde(default)]
    pub pages: SelectorSpec,
    /// 別名のキー。
    pub keys: Vec<String>,
    /// title も呼び名に含めるか。
    #[serde(default = "default_true")]
    pub include_title: bool,
}

/// 照合の規則。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MatchConfig {
    /// この文字数以下の ASCII 英数字の語は、大文字小文字を区別し単語として当てる。
    #[serde(default = "default_short_max")]
    pub short_max: usize,
}

impl Default for MatchConfig {
    fn default() -> Self {
        MatchConfig {
            short_max: default_short_max(),
        }
    }
}

/// 取り出し元のページの frontmatter の値で絞る。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustConfig {
    /// キー（例: credibility）。
    pub key: String,
    /// 許す値。
    pub allow: Vec<String>,
}

/// 検索の設定。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchConfig {
    /// 既定で検索しないページ（glob）。`--all` で含める。
    #[serde(default)]
    pub exclude: Vec<String>,
}

fn default_split() -> Vec<String> {
    vec!["、".to_string(), ",".to_string()]
}
fn default_true() -> bool {
    true
}
fn default_short_max() -> usize {
    3
}

impl Config {
    /// 文字列から読む。
    pub fn parse(src: &str, path: Option<&Path>) -> Result<Self> {
        toml::from_str(src).map_err(|e| Error::Config {
            path: path.map(Path::to_path_buf),
            message: e.to_string(),
        })
    }

    /// ファイルから読む。
    pub fn load(path: &Path) -> Result<Self> {
        let src = std::fs::read_to_string(path).map_err(|e| Error::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        Self::parse(&src, Some(path))
    }

    /// `explicit` があればそれを、なければ `<bundle>/okfast.toml` を読む。どちらもなければ既定値。
    pub fn discover(bundle: &Path, explicit: Option<&Path>) -> Result<(Self, Option<PathBuf>)> {
        if let Some(p) = explicit {
            return Ok((Self::load(p)?, Some(p.to_path_buf())));
        }
        let p = bundle.join(FILE_NAME);
        if p.is_file() {
            Ok((Self::load(&p)?, Some(p)))
        } else {
            Ok((Self::default(), None))
        }
    }
}
