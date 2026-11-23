//! 「読む範囲」を返す検索（`okfast search`）。
//!
//! 行単位の検索（[`crate::search`]）の結果を、見出しで区切った節にまとめ、BM25 で順位を付ける。
//!
//! - 結果の 1 件は節（[`crate::sections`]）。節が `max_lines` より長いときは、当たった行の前後 `window` 行を
//!   切り出す（重なる・隣り合う切り出しはつなぐ）
//! - 行の点は BM25。行を 1 つの文書とし、語ごとの idf（ありふれた語は軽い）と行の長さで決める。tf は 0/1
//! - 範囲の点は、語ごとに範囲の中で最も高い行の点を取り、足したもの（同じ語の繰り返しで長い節が上がりすぎない）
//! - 順位は、主題のページ（辞書の boost）→ 範囲に当たった語の種類が多い → 題名に当たったページ → 点、の順
//! - 上位 `top` 件（1 ページ `per_page` 件まで）のあとに、日付の新しい範囲を `recent` 件足す
//! - 必ず読む範囲（主題のページの、見出しが設定の語に当たる節と、決まったページの上位）を先頭に出す

use crate::aliases::Dictionary;
use crate::bundle::{Bundle, Page};
use crate::config::SearchConfig;
use crate::dates::{self, Date};
use crate::error::Result;
use crate::normalize::fold;
use crate::search::{self, FileHits, LineHit, Options, Query};
use crate::sections::heading_of;
use crate::selector::{compile_glob, compile_globset};
use rayon::prelude::*;
use std::collections::HashMap;

/// BM25 の k1。
const K1: f64 = 1.2;
/// BM25 の b。
const B: f64 = 0.75;

/// 検索の指定。
#[derive(Debug, Clone, Default)]
pub struct RangeOptions {
    /// 除外を無視してすべてのページを検索する。
    pub include_all: bool,
    /// 短い略語の最大文字数。
    pub short_max: usize,
    /// 関連度の上位から出す範囲の数（0 は無制限）。
    pub top: usize,
    /// 1 ページから出す範囲の数（0 は無制限）。
    pub per_page: usize,
    /// 足す新しい記録の数。
    pub recent: usize,
    /// この日付より前の範囲（と日付の分からない範囲）を出さない。必ず読む範囲には効かない。
    pub since: Option<Date>,
    /// 関連度ではなく日付の新しい順に並べる（新しい記録は足さない）。
    pub sort_date: bool,
}

/// 範囲の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// 必ず読む範囲。
    Must,
    /// 関連度（`sort_date` なら日付）の上位。
    Top,
    /// 足した新しい記録。
    Recent,
}

/// 範囲の中の当たった行。
#[derive(Debug, Clone)]
pub struct RangeHit<'a> {
    /// ファイル上の行番号（1 始まり）。
    pub line: usize,
    /// 行の内容。
    pub text: &'a str,
    /// 当たった語（ビット i が語 i）。
    pub terms: u64,
    /// BM25 の点。
    pub score: f64,
    /// 行の日付。
    pub date: Option<Date>,
}

/// 読む範囲。
#[derive(Debug, Clone)]
pub struct Range<'a> {
    /// ページの相対パス。
    pub rel: &'a str,
    /// ページ。
    pub page: &'a Page,
    /// 最初の行（ファイル上、1 始まり）。
    pub from: usize,
    /// 最後の行（ファイル上、この行を含む）。
    pub to: usize,
    /// 節の見出し（最初の見出しより前なら `None`）。
    pub heading: Option<&'a str>,
    /// 節全体の（最初の行, 最後の行）（ファイル上）。
    pub section: (usize, usize),
    /// 節の一部だけを切り出したか。
    pub partial: bool,
    /// 当たった行（行の順）。
    pub hits: Vec<RangeHit<'a>>,
    /// 当たった語（ビット i が語 i）。
    pub terms: u64,
    /// 点。
    pub score: f64,
    /// 日付（範囲の行の日付のうち最も新しいもの）。
    pub date: Option<Date>,
    /// 主題のページ（辞書の boost）の範囲か。
    pub boosted: bool,
    /// 題名に語が当たったページの範囲か。
    pub title_hit: bool,
    /// 種類。
    pub kind: Kind,
}

impl Range<'_> {
    fn overlaps(&self, o: &Range<'_>) -> bool {
        self.rel == o.rel && self.from <= o.to && o.from <= self.to
    }
}

/// 検索の結果。
#[derive(Debug, Clone)]
pub struct RangeResult<'a> {
    /// 出す範囲（必ず読む範囲 → 上位 → 新しい記録の順）。
    pub ranges: Vec<Range<'a>>,
    /// 出さなかった範囲（順位順）。
    pub rest: Vec<Range<'a>>,
    /// 当たった行の数（`min_chars` で除いたあと）。
    pub total_lines: usize,
    /// 当たったページの数。
    pub total_pages: usize,
    /// 語ごとの当たった行の数。
    pub lines_per_term: Vec<usize>,
    /// 主題のページ（当たったもの）。
    pub boosted: Vec<&'a str>,
    /// 検索したページ数。
    pub searched: usize,
    /// 除外したページ数。
    pub excluded: usize,
    /// 日付の取り出し元が設定されているか。
    pub dated: bool,
}

impl RangeResult<'_> {
    /// 候補の範囲の数（必ず読む範囲を除く）。
    pub fn total_ranges(&self) -> usize {
        self.ranges.iter().filter(|x| x.kind != Kind::Must).count() + self.rest.len()
    }
}

/// 1 ページの点を付けた当たり。
struct PageHits<'a> {
    rel: &'a str,
    page: &'a Page,
    boosted: bool,
    title_hit: bool,
    hits: Vec<RangeHit<'a>>,
    /// 語ごとの点（hits と同じ順。語 i の点は `per_term[k][i]`、当たっていなければ 0）。
    per_term: Vec<Vec<f64>>,
    /// 本文の各行の日付（日付の取り出し元がなければ `None`）。
    dates: Option<dates::LineDates>,
    excluded: bool,
}

/// バンドルを検索して、読む範囲を返す。
pub fn search_ranges<'a>(
    bundle: &'a Bundle,
    dict: &Dictionary,
    q: &Query,
    cfg: &SearchConfig,
    opt: &RangeOptions,
) -> Result<RangeResult<'a>> {
    let exclude = compile_globset(&cfg.exclude)?;
    let is_excluded = |rel: &str| !opt.include_all && exclude.is_match(rel);
    // 決まったページは exclude に当たっても見るので、そのときは全部を検索して自分で除く
    let need_all = !opt.include_all && !cfg.must_read.pages.is_empty();
    let r = search::search(
        bundle,
        dict,
        q,
        &Options {
            exclude: if need_all {
                Vec::new()
            } else {
                cfg.exclude.clone()
            },
            include_all: opt.include_all,
            short_max: opt.short_max,
        },
    )?;
    let (searched, excluded) = if need_all {
        let ex = bundle.pages.iter().filter(|p| is_excluded(&p.rel)).count();
        (bundle.pages.len() - ex, ex)
    } else {
        (r.searched, r.excluded)
    };

    // BM25 の材料: 検索したページの行の数と、正規化した行の長さの平均
    let (mut n_lines, mut n_bytes) = (0usize, 0usize);
    for p in bundle.pages.iter().filter(|p| !is_excluded(&p.rel)) {
        let ix = p.text_index();
        n_lines += ix.lines();
        n_bytes += ix.folded().len() - ix.lines();
    }
    let avgdl = if n_lines == 0 {
        1.0
    } else {
        (n_bytes as f64 / n_lines as f64).max(1.0)
    };

    let n_terms = q.terms.len();
    let min_chars = cfg.sections.min_chars;
    let keep = |h: &LineHit<'_>| {
        min_chars == 0 || h.text.trim().chars().count() >= min_chars || heading_of(h.text).is_some()
    };
    let mut df = vec![0usize; n_terms];
    let mut kept: Vec<(&FileHits<'a>, Vec<&LineHit<'a>>, bool)> = Vec::new();
    for f in &r.files {
        let hs: Vec<&LineHit<'a>> = f.hits.iter().filter(|h| keep(h)).collect();
        if hs.is_empty() {
            continue;
        }
        let ex = is_excluded(f.rel);
        if !ex {
            for h in &hs {
                for (i, c) in df.iter_mut().enumerate() {
                    if h.terms & (1 << i) != 0 {
                        *c += 1;
                    }
                }
            }
        }
        kept.push((f, hs, ex));
    }
    let n = n_lines as f64;
    let idf: Vec<f64> = df
        .iter()
        .map(|&d| {
            let d = d as f64;
            (1.0 + (n - d + 0.5) / (d + 0.5)).ln()
        })
        .collect();

    let sources = cfg.recent.sources();
    let pages: Vec<PageHits<'a>> = kept
        .into_par_iter()
        .map(|(f, hs, ex)| {
            let page = f.page;
            let ix = page.text_index();
            let first = page.body_line();
            let line_dates = (!sources.is_empty())
                .then(|| dates::cached_line_dates(page, &sources, &cfg.recent.table_columns));
            let mut per_term = Vec::with_capacity(hs.len());
            let hits = hs
                .iter()
                .map(|h| {
                    let i = h.line - first;
                    let (s, e) = ix.folded_line_range(i);
                    let dl = (e - s) as f64;
                    let norm = (K1 + 1.0) / (1.0 + K1 * (1.0 - B + B * dl / avgdl));
                    let scores: Vec<f64> = (0..n_terms)
                        .map(|t| {
                            if h.terms & (1 << t) != 0 {
                                idf[t] * norm
                            } else {
                                0.0
                            }
                        })
                        .collect();
                    let score = scores.iter().sum();
                    per_term.push(scores);
                    RangeHit {
                        line: h.line,
                        text: h.text,
                        terms: h.terms,
                        score,
                        date: line_dates.as_ref().and_then(|d| d[i]),
                    }
                })
                .collect();
            PageHits {
                rel: f.rel,
                page,
                boosted: f.boosted,
                title_hit: f.title_terms != 0,
                hits,
                per_term,
                dates: line_dates,
                excluded: ex,
            }
        })
        .collect();

    let mut lines_per_term = vec![0usize; n_terms];
    for p in pages.iter().filter(|p| !p.excluded) {
        for h in &p.hits {
            for (i, c) in lines_per_term.iter_mut().enumerate() {
                if h.terms & (1 << i) != 0 {
                    *c += 1;
                }
            }
        }
    }

    // 必ず読む範囲
    let mut must: Vec<Range<'a>> = Vec::new();
    let words: Vec<String> = cfg
        .must_read
        .headings
        .iter()
        .map(|w| fold(w))
        .filter(|w| !w.is_empty())
        .collect();
    if !words.is_empty() && cfg.must_read.topics > 0 {
        let topics = pages
            .iter()
            .filter(|p| !p.excluded && (p.boosted || p.title_hit))
            .take(cfg.must_read.topics);
        for p in topics {
            let o = p.page.outline();
            for s in &o.sections {
                let Some(h) = s.heading else { continue };
                let text = fold(&o.headings[h].text);
                if !words.iter().any(|w| text.contains(w.as_str())) {
                    continue;
                }
                let max = cfg.must_read.max_lines.max(1);
                let end = s.end.min(s.start + max - 1);
                let mut rg = make_range(p, s.start, end, s, n_terms);
                rg.partial = end < s.end;
                rg.kind = Kind::Must;
                push_unique(&mut must, rg);
            }
        }
    }
    let ranges_of = |p: &PageHits<'a>| -> Vec<Range<'a>> {
        page_ranges(p, cfg.sections.max_lines, cfg.sections.window, n_terms)
    };
    for mp in &cfg.must_read.pages {
        let g = compile_glob(&mp.path)?.compile_matcher();
        let mut found: Vec<Range<'a>> = pages
            .iter()
            .filter(|p| g.is_match(p.rel))
            .flat_map(&ranges_of)
            .collect();
        found.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.from.cmp(&b.from)));
        for mut rg in found.into_iter().take(mp.top) {
            rg.kind = Kind::Must;
            push_unique(&mut must, rg);
        }
    }

    // 候補
    let boost = q.boost();
    let mut cands: Vec<Range<'a>> = pages
        .iter()
        .filter(|p| !p.excluded)
        .flat_map(&ranges_of)
        .filter(|c| !must.iter().any(|m| m.overlaps(c)))
        .collect();
    let total_ranges_pages = pages.iter().filter(|p| !p.excluded).count();
    if let Some(since) = opt.since {
        cands.retain(|c| c.date.is_some_and(|d| d >= since));
    }
    let boost_pos = |rel: &str| boost.iter().position(|b| b == rel).unwrap_or(usize::MAX);
    if opt.sort_date {
        cands.sort_by(|a, b| {
            b.date
                .cmp(&a.date)
                .then(b.score.total_cmp(&a.score))
                .then(a.rel.cmp(b.rel))
                .then(a.from.cmp(&b.from))
        });
    } else {
        cands.sort_by(|a, b| {
            boost_pos(a.rel)
                .cmp(&boost_pos(b.rel))
                .then(b.terms.count_ones().cmp(&a.terms.count_ones()))
                .then(b.title_hit.cmp(&a.title_hit))
                .then(b.score.total_cmp(&a.score))
                .then(a.rel.cmp(b.rel))
                .then(a.from.cmp(&b.from))
        });
    }

    let limit = |n: usize| if n == 0 { usize::MAX } else { n };
    let (top, per_page) = (limit(opt.top), limit(opt.per_page));
    let mut chosen = vec![false; cands.len()];
    let mut per: HashMap<&str, usize> = HashMap::new();
    let mut n_top = 0;
    for (i, c) in cands.iter().enumerate() {
        if n_top >= top {
            break;
        }
        let k = per.entry(c.rel).or_default();
        if *k >= per_page {
            continue;
        }
        *k += 1;
        chosen[i] = true;
        n_top += 1;
    }
    let mut recent_idx: Vec<usize> = Vec::new();
    if !opt.sort_date && opt.recent > 0 {
        let mut idx: Vec<usize> = (0..cands.len())
            .filter(|&i| !chosen[i] && cands[i].date.is_some())
            .collect();
        idx.sort_by(|&a, &b| {
            cands[b]
                .date
                .cmp(&cands[a].date)
                .then(cands[b].score.total_cmp(&cands[a].score))
                .then(a.cmp(&b))
        });
        let mut per: HashMap<&str, usize> = HashMap::new();
        for i in idx {
            if recent_idx.len() >= opt.recent {
                break;
            }
            let k = per.entry(cands[i].rel).or_default();
            if *k >= per_page {
                continue;
            }
            *k += 1;
            recent_idx.push(i);
        }
    }

    let mut ranges = must;
    let mut rest = Vec::new();
    let mut recent: Vec<Option<Range<'a>>> = vec![None; recent_idx.len()];
    for (i, mut c) in cands.into_iter().enumerate() {
        if chosen[i] {
            c.kind = Kind::Top;
            ranges.push(c);
        } else if let Some(k) = recent_idx.iter().position(|&x| x == i) {
            c.kind = Kind::Recent;
            recent[k] = Some(c);
        } else {
            rest.push(c);
        }
    }
    ranges.extend(recent.into_iter().flatten());

    Ok(RangeResult {
        ranges,
        rest,
        total_lines: pages
            .iter()
            .filter(|p| !p.excluded)
            .map(|p| p.hits.len())
            .sum(),
        total_pages: total_ranges_pages,
        lines_per_term,
        boosted: pages
            .iter()
            .filter(|p| !p.excluded && p.boosted)
            .map(|p| p.rel)
            .collect(),
        searched,
        excluded,
        dated: !sources.is_empty(),
    })
}

fn push_unique<'a>(v: &mut Vec<Range<'a>>, rg: Range<'a>) {
    if !v.iter().any(|m| m.overlaps(&rg)) {
        v.push(rg);
    }
}

/// ページの当たりを節・切り出しの範囲にまとめる。
fn page_ranges<'a>(
    p: &PageHits<'a>,
    max_lines: usize,
    window: usize,
    n_terms: usize,
) -> Vec<Range<'a>> {
    let page = p.page;
    let o = page.outline();
    let first = page.body_line();
    let mut out = Vec::new();
    let mut k = 0;
    while k < p.hits.len() {
        let line = p.hits[k].line - first;
        let Some(si) = o.section_of(line) else { break };
        let s = &o.sections[si];
        let next = o.sections.get(si + 1).map_or(usize::MAX, |x| x.start);
        let mut j = k;
        while j < p.hits.len() && p.hits[j].line - first < next {
            j += 1;
        }
        if s.len() <= max_lines.max(1) {
            out.push(make_range(p, s.start, s.end.max(line), s, n_terms));
        } else {
            // 当たった行の前後を切り出し、重なる・隣り合うものはつなぐ
            let mut cur: Option<(usize, usize)> = None;
            for h in &p.hits[k..j] {
                let l = h.line - first;
                let w = (
                    l.saturating_sub(window).max(s.start),
                    (l + window).min(s.end).max(l),
                );
                cur = match cur {
                    Some((a, b)) if w.0 <= b + 1 => Some((a, b.max(w.1))),
                    Some((a, b)) => {
                        let mut rg = make_range(p, a, b, s, n_terms);
                        rg.partial = true;
                        out.push(rg);
                        Some(w)
                    }
                    None => Some(w),
                };
            }
            if let Some((a, b)) = cur {
                let mut rg = make_range(p, a, b, s, n_terms);
                rg.partial = true;
                out.push(rg);
            }
        }
        k = j;
    }
    out
}

/// 本文の行 `start..=end`（0 始まり）の範囲を作る。点は範囲の中の当たりから決める。
fn make_range<'a>(
    p: &PageHits<'a>,
    start: usize,
    end: usize,
    s: &crate::sections::Section,
    n_terms: usize,
) -> Range<'a> {
    let page = p.page;
    let first = page.body_line();
    let (from, to) = (first + start, first + end);
    let lo = p.hits.partition_point(|h| h.line < from);
    let hi = p.hits.partition_point(|h| h.line <= to);
    let mut best = vec![0.0f64; n_terms];
    for row in &p.per_term[lo..hi] {
        for (b, &v) in best.iter_mut().zip(row) {
            if v > *b {
                *b = v;
            }
        }
    }
    let hits: Vec<RangeHit<'a>> = p.hits[lo..hi].to_vec();
    let o = page.outline();
    Range {
        rel: p.rel,
        page,
        from,
        to,
        heading: s.heading.map(|h| o.headings[h].text.as_str()),
        section: (first + s.start, first + s.end),
        partial: false,
        terms: hits.iter().fold(0, |a, h| a | h.terms),
        date: p
            .dates
            .as_ref()
            .and_then(|d| d[start..=end].iter().flatten().max().copied()),
        score: best.iter().sum(),
        hits,
        boosted: p.boosted,
        title_hit: p.title_hit,
        kind: Kind::Top,
    }
}
