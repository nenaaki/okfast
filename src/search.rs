//! 行単位の検索（同義語の言い換えつき）。
//!
//! - 語ごとに「言い換えのどれかに当たる行」を探し、どの語にも当たらない行は出さない（語同士は OR）。
//! - 通常の語は正規化（NFKC・小文字・空白除去）した行に対して Aho-Corasick で一度に探す。
//! - 短い略語は NFKC だけの行に対して、大文字小文字を区別し単語として探す。
//! - 条件付きの呼び名は、`when` に当たるページでだけ使う。照合器は条件の組み合わせごとに 1 回だけ作る。

use crate::aliases::{Dictionary, Expansion};
use crate::bundle::{Bundle, Page};
use crate::error::{Error, Result};
use crate::normalize::{fold, is_short, nfkc};
use crate::selector::compile_globset;
use aho_corasick::{AhoCorasick, Input, MatchKind, Span, packed};
use memchr::memmem;
use rayon::prelude::*;
use std::collections::HashMap;

/// 検索する 1 語。
#[derive(Debug, Clone)]
pub struct Term {
    /// 指定されたままの語。
    pub label: String,
    /// 利用者が `|` で並べた言い換え。
    pub alts: Vec<String>,
    /// 辞書による言い換え。
    pub expansion: Expansion,
}

/// 検索の問い。
#[derive(Debug, Clone)]
pub struct Query {
    /// 語。
    pub terms: Vec<Term>,
}

impl Query {
    /// `"A|B" "C"` のような語の並びから作る。`dict` があれば言い換える。
    pub fn parse(words: &[String], dict: Option<&Dictionary>) -> Result<Self> {
        let terms: Vec<Term> = words
            .iter()
            .map(|w| {
                let alts: Vec<String> = w
                    .split('|')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect();
                let expansion = dict.map(|d| d.expand(&alts)).unwrap_or_default();
                Term {
                    label: w.clone(),
                    alts,
                    expansion,
                }
            })
            .filter(|t| !t.alts.is_empty())
            .collect();
        if terms.is_empty() {
            return Err(Error::Query("検索語がない".into()));
        }
        Ok(Query { terms })
    }

    /// 主題のページ（言い換えで分かったもの）。
    pub fn boost(&self) -> Vec<String> {
        let mut v: Vec<String> = Vec::new();
        for t in &self.terms {
            for b in &t.expansion.boost {
                if !v.contains(b) {
                    v.push(b.clone());
                }
            }
        }
        v
    }
}

/// 検索の指定。
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// 既定で除くページ（glob）。
    pub exclude: Vec<String>,
    /// 除外を無視してすべてのページを検索する。
    pub include_all: bool,
    /// 短い略語の最大文字数。
    pub short_max: usize,
    /// 空でなければ、この glob に当たるページだけを検索する（`exclude` に当たるページも含める）。
    pub paths: Vec<String>,
    /// 主題のページかを決める frontmatter のキー（[`FileHits::topic_terms`]）。空なら調べない。
    pub topic_keys: Vec<String>,
    /// 脚注の定義の行（`[^id]: …`）を当たりにしない。
    pub skip_footnotes: bool,
    /// リンクの行き先（`](…)` の中）だけに当たった語を数えない。
    pub skip_link_targets: bool,
}

/// 当たった行。
#[derive(Debug, Clone)]
pub struct LineHit<'a> {
    /// 行番号（1 始まり）。
    pub line: usize,
    /// 行の内容（バンドルの本文を借りる）。
    pub text: &'a str,
    /// 当たった語（ビット i が語 i）。
    pub terms: u64,
}

/// 1 ページの結果。
#[derive(Debug, Clone)]
pub struct FileHits<'a> {
    /// 相対パス。
    pub rel: &'a str,
    /// ページ（題名・前後の行を引くため）。
    pub page: &'a Page,
    /// 主題のページか。
    pub boosted: bool,
    /// 当たった行。
    pub hits: Vec<LineHit<'a>>,
    /// ページのどこかに当たった語（ビット i が語 i。題名を含む）。
    pub terms: u64,
    /// 題名に当たった語。
    pub title_terms: u64,
    /// `topic_keys` の値に当たった語（[`Options::topic_keys`] が空なら 0）。
    pub topic_terms: u64,
    /// 当たった行のうち、見出し（`#` で始まる行）の数。
    pub heading_hits: usize,
}

impl FileHits<'_> {
    /// 順位の鍵（小さいほど上）。主題のページの順は別に扱う。
    fn rank_key(&self) -> impl Ord {
        (
            std::cmp::Reverse(self.terms.count_ones()),
            std::cmp::Reverse(self.title_terms.count_ones()),
            std::cmp::Reverse(self.heading_hits),
            std::cmp::Reverse(self.hits.len()),
        )
    }
}

/// 検索の結果。
#[derive(Debug, Clone)]
pub struct SearchResult<'a> {
    /// ページごとの結果。順位は、主題のページ → 当たった語の種類が多い → 題名に当たった語が多い
    /// → 見出しに当たった数が多い → 当たった行が多い → 相対パス、の順。
    pub files: Vec<FileHits<'a>>,
    /// 検索したページ数。
    pub searched: usize,
    /// 除外したページ数。
    pub excluded: usize,
}

impl SearchResult<'_> {
    /// 当たった行の総数。
    pub fn total_lines(&self) -> usize {
        self.files.iter().map(|f| f.hits.len()).sum()
    }

    /// 語ごとの当たった行数。
    pub fn lines_per_term(&self, n: usize) -> Vec<usize> {
        let mut v = vec![0; n];
        for h in self.files.iter().flat_map(|f| &f.hits) {
            for (i, c) in v.iter_mut().enumerate() {
                if h.terms & (1 << i) != 0 {
                    *c += 1;
                }
            }
        }
        v
    }
}

/// 1 スレッドに任せる本文の量の目安。
const BYTES_PER_THREAD: usize = 4 << 20;

/// 当たりそうな行を素早く見つけるもの。見つけた行だけ、すべての語を確かめる。
///
/// Aho-Corasick の自動の選択は、日本語の語が少ないときに英語向けの「珍しいバイト」による絞り込みを
/// 選んでしまい、約 1 GB/s まで落ちる。語の数で SIMD の探し方を選び直す。
enum Locator {
    /// 1 語: memchr の memmem。
    One(memmem::Finder<'static>),
    /// 2〜64 語: Teddy（SIMD で複数の語を同時に探す）。
    Packed(packed::Searcher),
    /// それより多い・SIMD が使えない: Aho-Corasick。
    Ac(AhoCorasick),
}

impl Locator {
    fn build(pats: &[String]) -> Result<Self> {
        if let [one] = pats {
            return Ok(Locator::One(
                memmem::Finder::new(one.as_bytes()).into_owned(),
            ));
        }
        if let Some(s) = packed::Config::new().builder().extend(pats).build() {
            return Ok(Locator::Packed(s));
        }
        AhoCorasick::builder()
            .match_kind(MatchKind::LeftmostFirst)
            .build(pats)
            .map(Locator::Ac)
            .map_err(|e| Error::Query(format!("照合器を作れない: {e}")))
    }

    /// `from` 以降で最初に当たる位置。
    #[inline]
    fn find(&self, hay: &str, from: usize) -> Option<usize> {
        match self {
            Locator::One(f) => f.find(&hay.as_bytes()[from..]).map(|p| p + from),
            Locator::Packed(s) => s
                .find_in(hay, Span::from(from..hay.len()))
                .map(|m| m.start()),
            Locator::Ac(a) => a
                .find(Input::new(hay).span(from..hay.len()))
                .map(|m| m.start()),
        }
    }
}

struct Matcher {
    locator: Option<Locator>,
    /// 行の中のすべての語を確かめる（重なりも数える）。
    ac: Option<AhoCorasick>,
    ac_terms: Vec<u64>,
    shorts: Vec<(memmem::Finder<'static>, u64)>,
}

impl Matcher {
    fn build(q: &Query, when_mask: u64, short_max: usize) -> Result<Self> {
        let mut long: HashMap<String, u64> = HashMap::new();
        let mut shorts: HashMap<String, u64> = HashMap::new();
        for (i, t) in q.terms.iter().enumerate() {
            let bit = 1u64 << i;
            let cond = t
                .expansion
                .conditional
                .iter()
                .filter(|c| when_mask & (1 << c.when) != 0)
                .flat_map(|c| &c.names);
            for a in t.alts.iter().chain(&t.expansion.added).chain(cond) {
                if is_short(a, short_max) {
                    *shorts.entry(nfkc(a).trim().to_string()).or_default() |= bit;
                } else {
                    let f = fold(a);
                    if !f.is_empty() {
                        *long.entry(f).or_default() |= bit;
                    }
                }
            }
        }
        let (pats, ac_terms): (Vec<String>, Vec<u64>) = long.into_iter().unzip();
        let (locator, ac) = if pats.is_empty() {
            (None, None)
        } else {
            let ac = AhoCorasick::new(&pats)
                .map_err(|e| Error::Query(format!("照合器を作れない: {e}")))?;
            (Some(Locator::build(&pats)?), Some(ac))
        };
        Ok(Matcher {
            locator,
            ac,
            ac_terms,
            shorts: shorts
                .into_iter()
                .map(|(s, bit)| (memmem::Finder::new(s.as_bytes()).into_owned(), bit))
                .collect(),
        })
    }

    /// 任意の文字列（題名など）に当たる語。
    fn text_mask(&self, s: &str) -> u64 {
        let mut mask = 0;
        if let Some(ac) = &self.ac {
            for m in ac.find_overlapping_iter(&fold(s)) {
                mask |= self.ac_terms[m.pattern().as_usize()];
            }
        }
        if !self.shorts.is_empty() {
            let n = nfkc(s);
            let n = n.as_bytes();
            for (finder, bit) in &self.shorts {
                let len = finder.needle().len();
                if finder.find_iter(n).any(|start| {
                    let end = start + len;
                    (start == 0 || !n[start - 1].is_ascii_alphanumeric())
                        && (end == n.len() || !n[end].is_ascii_alphanumeric())
                }) {
                    mask |= bit;
                }
            }
        }
        mask
    }

    /// 当たった行の語を、本文として数えるものに絞る（脚注の定義の行・リンクの行き先を除く）。
    /// 残らなければ `None`。
    #[inline]
    fn body_terms(&self, text: &str, terms: u64, opt: &Options) -> Option<u64> {
        if opt.skip_footnotes && crate::markdown::footnote_def(text).is_some() {
            return None;
        }
        let terms = if opt.skip_link_targets && text.contains("](") {
            self.text_mask(&crate::markdown::strip_link_targets(text)) & terms
        } else {
            terms
        };
        (terms != 0).then_some(terms)
    }

    /// ページの当たった行を（0 始まりの行, 語のビット）で返す。行の昇順・重複なし。
    fn page_hits(&self, page: &Page, found: &mut Vec<(u32, u64)>) {
        found.clear();
        let ix = page.text_index();
        if let (Some(loc), Some(ac)) = (&self.locator, &self.ac) {
            let f = ix.folded();
            let mut pos = 0;
            while let Some(at) = loc.find(f, pos) {
                let line = ix.folded_line(at);
                let (ls, le) = ix.folded_line_range(line);
                let mut mask = 0;
                for m in ac.find_overlapping_iter(&f[ls..le]) {
                    mask |= self.ac_terms[m.pattern().as_usize()];
                }
                found.push((line as u32, mask));
                pos = le + 1;
                if pos >= f.len() {
                    break;
                }
            }
        }
        if !self.shorts.is_empty() {
            let n = ix.nfkc(page.body()).as_bytes();
            for (finder, bit) in &self.shorts {
                let len = finder.needle().len();
                for start in finder.find_iter(n) {
                    let end = start + len;
                    let before_ok = start == 0 || !n[start - 1].is_ascii_alphanumeric();
                    let after_ok = end == n.len() || !n[end].is_ascii_alphanumeric();
                    if before_ok && after_ok {
                        found.push((ix.nfkc_line(start) as u32, *bit));
                    }
                }
            }
        }
        if found.len() > 1 {
            found.sort_unstable_by_key(|h| h.0);
            found.dedup_by(|b, a| {
                let same = a.0 == b.0;
                if same {
                    a.1 |= b.1;
                }
                same
            });
        }
    }
}

/// バンドルを検索する。
pub fn search<'a>(
    bundle: &'a Bundle,
    dict: &Dictionary,
    q: &Query,
    opt: &Options,
) -> Result<SearchResult<'a>> {
    if q.terms.len() > 64 {
        return Err(Error::Query("検索語は 64 個まで".into()));
    }
    let exclude = compile_globset(&opt.exclude)?;
    let paths = compile_globset(&opt.paths)?;
    let targets: Vec<_> = bundle
        .pages
        .iter()
        .filter(|p| {
            if opt.paths.is_empty() {
                opt.include_all || !exclude.is_match(&p.rel)
            } else {
                paths.is_match(&p.rel)
            }
        })
        .collect();
    let excluded = bundle.pages.len() - targets.len();

    let whens = dict.whens();
    if whens.len() > 64 {
        return Err(Error::Query("条件付きの列は 64 個まで".into()));
    }
    let masks: Vec<u64> = targets
        .iter()
        .map(|p| {
            whens
                .iter()
                .enumerate()
                .filter(|(_, w)| w.matches(&p.rel, p.frontmatter()))
                .fold(0u64, |m, (i, _)| m | (1 << i))
        })
        .collect();
    let mut matchers: HashMap<u64, Matcher> = HashMap::new();
    for &m in &masks {
        if let std::collections::hash_map::Entry::Vacant(e) = matchers.entry(m) {
            e.insert(Matcher::build(q, m, opt.short_max)?);
        }
    }

    let boost = q.boost();
    let one_page = |page: &'a Page, mask: &u64, found: &mut Vec<(u32, u64)>| {
        let m = &matchers[mask];
        m.page_hits(page, found);
        if found.is_empty() {
            return None;
        }
        let ix = page.text_index();
        let body = page.body();
        let first = page.body_line();
        let hits: Vec<LineHit<'a>> = found
            .iter()
            .filter_map(|&(i, terms)| {
                let text = ix.line(body, i as usize);
                let terms = m.body_terms(text, terms, opt)?;
                Some(LineHit {
                    line: first + i as usize,
                    text,
                    terms,
                })
            })
            .collect();
        if hits.is_empty() {
            return None;
        }
        let title_terms = m.text_mask(&page.title());
        let topic_terms = opt.topic_keys.iter().fold(0, |acc, k| {
            if k == "title" {
                acc | title_terms
            } else {
                page.frontmatter()
                    .strings(k)
                    .iter()
                    .fold(acc, |a, v| a | m.text_mask(v))
            }
        });
        Some(FileHits {
            rel: page.rel.as_str(),
            page,
            boosted: boost.contains(&page.rel),
            terms: hits.iter().fold(title_terms, |a, h| a | h.terms),
            title_terms,
            topic_terms,
            heading_hits: hits
                .iter()
                .filter(|h| h.text.trim_start().starts_with('#'))
                .count(),
            hits,
        })
    };

    // スレッドを起こす手間は、仕事が小さいと照合そのものより高くつく（特に WSL）。
    // 本文の量からスレッド数を決め、ページを本文の量が均等な連続した塊に分けて 1 塊 1 仕事にする。
    let bytes: usize = targets.iter().map(|p| p.body().len()).sum();
    let par = (bytes / BYTES_PER_THREAD).clamp(1, rayon::current_num_threads());
    let mut files: Vec<FileHits<'a>> = if par == 1 {
        let mut found = Vec::new();
        targets
            .iter()
            .zip(&masks)
            .filter_map(|(p, m)| one_page(p, m, &mut found))
            .collect()
    } else {
        let mut bounds = vec![0usize];
        let per = bytes.div_ceil(par);
        let mut acc = 0;
        for (i, p) in targets.iter().enumerate() {
            acc += p.body().len();
            if acc >= per * bounds.len() && bounds.len() < par {
                bounds.push(i + 1);
            }
        }
        bounds.push(targets.len());
        bounds.dedup();
        bounds
            .par_windows(2)
            .flat_map_iter(|w| {
                let mut found = Vec::new();
                (w[0]..w[1])
                    .filter_map(|i| one_page(targets[i], &masks[i], &mut found))
                    .collect::<Vec<_>>()
            })
            .collect()
    };

    files.sort_by(|a, b| {
        let ka = boost.iter().position(|x| *x == a.rel).unwrap_or(usize::MAX);
        let kb = boost.iter().position(|x| *x == b.rel).unwrap_or(usize::MAX);
        ka.cmp(&kb)
            .then_with(|| a.rank_key().cmp(&b.rank_key()))
            .then_with(|| a.rel.cmp(b.rel))
    });
    Ok(SearchResult {
        files,
        searched: targets.len(),
        excluded,
    })
}
