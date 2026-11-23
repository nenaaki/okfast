//! 設定ファイル `okfast.toml`（バンドルの root に置く）。
//!
//! 同義語の取り出し方はバンドルごとに宣言する。okfast 本体は特定のバンドルの約束を持たない。

use crate::dates::DateSource;
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
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchConfig {
    /// 既定で検索しないページ（glob）。`--all` で含める。
    #[serde(default)]
    pub exclude: Vec<String>,
    /// `search` で主題のページかを決める frontmatter のキー。値に語が当たったページの範囲は点が上がる
    /// （`title` がないページはファイル名を使う）。
    #[serde(default = "default_topic_keys")]
    pub topic_keys: Vec<String>,
    /// `search` の範囲の見出しに値を出す frontmatter のキー（例: `["credibility"]`）。
    #[serde(default)]
    pub show_keys: Vec<String>,
    /// `search` の節の切り方。
    #[serde(default)]
    pub sections: SectionsConfig,
    /// `search` で足す新しい記録と、行の日付の取り方。
    #[serde(default)]
    pub recent: RecentConfig,
    /// `search` で必ず出す範囲。
    #[serde(default)]
    pub must_read: MustReadConfig,
}

impl Default for SearchConfig {
    fn default() -> Self {
        SearchConfig {
            exclude: Vec::new(),
            topic_keys: default_topic_keys(),
            show_keys: Vec::new(),
            sections: SectionsConfig::default(),
            recent: RecentConfig::default(),
            must_read: MustReadConfig::default(),
        }
    }
}

/// 範囲の点の決め方。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SectionScore {
    /// 当たった行の点を高い順に並べ、1・1/2・1/4… を掛けて足す（何行も当たる範囲が少し上がる）。
    #[default]
    Decay,
    /// 語ごとに範囲の中で最も高い行の点を取り、足す（同じ語の繰り返しを数えない）。
    Max,
}

/// `search` の節の切り方。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SectionsConfig {
    /// これより長い節は、当たった行の前後だけを範囲にする。
    #[serde(default = "default_section_max_lines")]
    pub max_lines: usize,
    /// 長い節で、当たった行の前後に含める行の数。
    #[serde(default = "default_window")]
    pub window: usize,
    /// 本文（前後の空白を除く）がこの文字数より短い行は、当たりに数えない（見出しの行は数える）。
    #[serde(default)]
    pub min_chars: usize,
    /// 範囲の点の決め方。
    #[serde(default)]
    pub score: SectionScore,
    /// 脚注の定義の行（`[^id]: 出典`）を当たりに数えない（日付の取り出し元としては使う）。
    #[serde(default = "default_true")]
    pub skip_footnotes: bool,
}

impl Default for SectionsConfig {
    fn default() -> Self {
        SectionsConfig {
            max_lines: default_section_max_lines(),
            window: default_window(),
            min_chars: 0,
            score: SectionScore::default(),
            skip_footnotes: true,
        }
    }
}

/// `search` で足す新しい記録と、行の日付の取り方。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecentConfig {
    /// 関連度の上位のあとに足す、日付の新しい範囲の数。
    #[serde(default = "default_recent_count")]
    pub count: usize,
    /// 行の日付の取り出し元（先に書いたものから探す）。空なら日付を使わない。
    #[serde(default)]
    pub date_sources: Vec<String>,
    /// `table_row` で、1 列目より先に見る列の名前。
    #[serde(default)]
    pub table_columns: Vec<String>,
}

impl Default for RecentConfig {
    fn default() -> Self {
        RecentConfig {
            count: default_recent_count(),
            date_sources: Vec::new(),
            table_columns: Vec::new(),
        }
    }
}

impl RecentConfig {
    /// 日付の取り出し元（読めないものは設定を読むときにエラーにしてある）。
    pub fn sources(&self) -> Vec<DateSource> {
        self.date_sources
            .iter()
            .filter_map(|s| DateSource::parse(s))
            .collect()
    }
}

/// `search` で必ず出す範囲。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MustReadConfig {
    /// 見出しを探す主題のページの数（順位の上から）。
    #[serde(default = "default_must_topics")]
    pub topics: usize,
    /// 見出しにこの語のどれかを含む節を、必ず出す。
    #[serde(default)]
    pub headings: Vec<String>,
    /// 必ず出す節の行数の上限（超えたら先頭から）。
    #[serde(default = "default_must_max_lines")]
    pub max_lines: usize,
    /// 決まったページで、当たった範囲の上位を必ず出す。
    #[serde(default)]
    pub pages: Vec<MustReadPage>,
}

impl Default for MustReadConfig {
    fn default() -> Self {
        MustReadConfig {
            topics: default_must_topics(),
            headings: Vec::new(),
            max_lines: default_must_max_lines(),
            pages: Vec::new(),
        }
    }
}

/// 当たった範囲の上位を必ず出すページ。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MustReadPage {
    /// ページの glob。`exclude` に当たるページでもよい。
    pub path: String,
    /// 出す範囲の数。
    #[serde(default = "default_must_page_top")]
    pub top: usize,
    /// 当たった行の前後に含める行の数。指定すると節の長さによらず切り出す（0 なら当たった行だけ）。
    /// 省略すると `[search.sections]` と同じ切り方。
    #[serde(default)]
    pub window: Option<usize>,
}

fn default_topic_keys() -> Vec<String> {
    vec!["title".to_string()]
}
fn default_section_max_lines() -> usize {
    40
}
fn default_window() -> usize {
    4
}
fn default_recent_count() -> usize {
    4
}
fn default_must_topics() -> usize {
    2
}
fn default_must_max_lines() -> usize {
    60
}
fn default_must_page_top() -> usize {
    3
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
        let err = |message: String| Error::Config {
            path: path.map(Path::to_path_buf),
            message,
        };
        let c: Config = toml::from_str(src).map_err(|e| err(e.to_string()))?;
        for s in &c.search.recent.date_sources {
            if DateSource::parse(s).is_none() {
                return Err(err(format!(
                    "[search.recent] date_sources の「{s}」は読めない（table_row・frontmatter:<キー>・footnote_source・heading のどれか）"
                )));
            }
        }
        for p in &c.search.must_read.pages {
            crate::selector::compile_glob(&p.path)?;
        }
        Ok(c)
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
