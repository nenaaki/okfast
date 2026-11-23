//! 日付つきの行の時系列の一覧（`okfast search` の `timeline`）。
//!
//! - `search` が返した範囲（必ず読む範囲・関連度の上位・新しい記録）の行だけから作る。バンドル全体からは
//!   作らない（範囲の外の行を読ませる道具にしない）
//! - 行の日付は `[search.recent] date_sources` と同じ取り方（[`crate::dates::line_dates`]）。推測の日付は
//!   付けない
//! - 行の種類と変更の印は、設定の `[search.timeline]` のパターン（部分一致）で決める。okfast は行の意味を
//!   知らない
//! - 既定（`hits_only`）では、検索語に当たった行と変更の印のある行だけを出す（節を丸ごと返した範囲で、
//!   問いと関係ない日付つきの行が一覧を埋めないように）。変更先はすべての日付つきの行から探す
//! - 並びは日付の新しい順（同じ日付ならパス・行の順）

use crate::config::{SearchConfig, TimelineConfig};
use crate::dates::{self, Date};
use crate::markdown::footnote_def;
use crate::ranges::Range;
use std::collections::HashSet;

/// 変更の印のパターンで、変更先の日付を表す部分。
pub const DATE_SLOT: &str = "{date}";

/// 一覧の 1 行。
#[derive(Debug, Clone)]
pub struct Entry<'a> {
    /// 行の日付。
    pub date: Date,
    /// ページの相対パス。
    pub rel: &'a str,
    /// ファイル上の行番号（1 始まり）。
    pub line: usize,
    /// 行の種類（`[search.timeline] kinds` の名前。どれにも当たらなければ `None`）。
    pub kind: Option<&'a str>,
    /// 変更の印があれば、その中身。
    pub superseded: Option<Superseded<'a>>,
    /// 行の内容。
    pub text: &'a str,
}

/// 変更の印。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Superseded<'a> {
    /// 変更先の日付（印に `{date}` がない・読めないときは `None`）。
    pub date: Option<Date>,
    /// 同じ範囲の集まりの中の、変更先の日付の同じ種類の行（パス, 行番号）。
    pub target: Option<(&'a str, usize)>,
}

/// 時系列の一覧（日付の新しい順。`max` 行まで）。日付の取り方が設定にないか、`max` が 0 なら空。
pub fn timeline<'a>(ranges: &[Range<'a>], cfg: &'a SearchConfig, max: usize) -> Vec<Entry<'a>> {
    let tl = &cfg.timeline;
    let sources = cfg.recent.sources();
    if sources.is_empty() || max == 0 || !tl.is_declared() {
        return Vec::new();
    }
    let mut seen: HashSet<(&str, usize)> = HashSet::new();
    let mut all: Vec<Entry<'a>> = Vec::new();
    let hits: HashSet<(&str, usize)> = ranges
        .iter()
        .flat_map(|rg| rg.hits.iter().map(move |h| (rg.rel, h.line)))
        .collect();
    for rg in ranges {
        let page = rg.page;
        let line_dates = dates::cached_line_dates(page, &sources, &cfg.recent.table_columns);
        let base = dates::generated_at(page);
        for (line, text) in rg.body() {
            let Some(date) = line_dates[line - page.body_line()] else {
                continue;
            };
            if text.trim().is_empty() || footnote_def(text).is_some() || is_delimiter_row(text) {
                continue;
            }
            if !seen.insert((rg.rel, line)) {
                continue;
            }
            all.push(Entry {
                date,
                rel: rg.rel,
                line,
                kind: kind_of(tl, text),
                superseded: superseded_of(tl, text, base),
                text,
            });
        }
    }
    // 変更先: 同じ集まりの中の、その日付の同じ種類の行（種類のない行なら、種類の付いた行）
    let targets: Vec<Option<(&'a str, usize)>> = all
        .iter()
        .map(|e| {
            let d = e.superseded.as_ref()?.date?;
            all.iter()
                .filter(|x| x.date == d && x.superseded.is_none())
                .filter(|x| match e.kind {
                    Some(k) => x.kind == Some(k),
                    None => x.kind.is_some(),
                })
                .min_by(|a, b| a.rel.cmp(b.rel).then(a.line.cmp(&b.line)))
                .map(|x| (x.rel, x.line))
        })
        .collect();
    for (e, t) in all.iter_mut().zip(targets) {
        if let Some(s) = e.superseded.as_mut() {
            s.target = t;
        }
    }
    if tl.kinds_only {
        all.retain(|e| e.kind.is_some() || e.superseded.is_some());
    }
    if tl.hits_only {
        all.retain(|e| e.superseded.is_some() || hits.contains(&(e.rel, e.line)));
    }
    all.sort_by(|a, b| {
        b.date
            .cmp(&a.date)
            .then(a.rel.cmp(b.rel))
            .then(a.line.cmp(&b.line))
    });
    all.truncate(max);
    all
}

/// 行の種類（先に書いたものから）。
fn kind_of<'a>(tl: &'a TimelineConfig, text: &str) -> Option<&'a str> {
    tl.kinds
        .iter()
        .find(|k| text.contains(k.pattern.as_str()))
        .map(|k| k.name.as_str())
}

/// 行の変更の印（`superseded` のパターンを先に書いたものから）。
fn superseded_of<'a>(
    tl: &TimelineConfig,
    text: &str,
    base: Option<Date>,
) -> Option<Superseded<'a>> {
    tl.superseded
        .iter()
        .find_map(|p| read_mark(p, text, base).map(|date| Superseded { date, target: None }))
}

/// 行に変更の印のパターン `pattern` があれば、`Some(変更先の日付)` を返す（日付は `{date}` の部分が日付で
/// 始まるときだけ読む。年のない日付は `base`、つまりページの `generated.at` の年。読めなければ `Some(None)`）。
/// 印の部分が何度も出てくる行では、日付の読めるものを使う。
pub fn read_mark(pattern: &str, text: &str, base: Option<Date>) -> Option<Option<Date>> {
    let Some((pre, post)) = pattern.split_once(DATE_SLOT) else {
        return text.contains(pattern).then_some(None);
    };
    let mut found = false;
    if pre.is_empty() {
        // `{date}に変更` のように前の部分がなければ、後ろの部分の直前の日付の文字の並びを読む
        for (j, _) in text.match_indices(post) {
            found = true;
            let head = &text[..j];
            let start = head
                .rfind(|c: char| !(c.is_ascii_digit() || c == '-' || c == '/'))
                .map_or(0, |k| {
                    k + head[k..].chars().next().map_or(1, char::len_utf8)
                });
            if let Some(d) = dates::cell_date(&head[start..], base) {
                return Some(Some(d));
            }
        }
    } else {
        for (i, _) in text.match_indices(pre) {
            let rest = &text[i + pre.len()..];
            let slot = if post.is_empty() {
                Some(rest)
            } else {
                rest.find(post).map(|j| &rest[..j])
            };
            if let Some(slot) = slot {
                found = true;
                if let Some(d) = dates::cell_date(slot, base) {
                    return Some(Some(d));
                }
            }
        }
    }
    found.then_some(None)
}

/// 表の区切りの行（`|---|:--:|`）。
fn is_delimiter_row(text: &str) -> bool {
    let t = text.trim();
    t.starts_with('|')
        && t.contains('-')
        && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ' | '\t'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TimelineKind;

    fn cfg(superseded: &[&str]) -> TimelineConfig {
        TimelineConfig {
            kinds: vec![TimelineKind {
                name: "conclusion".into(),
                pattern: "【結論】".into(),
            }],
            superseded: superseded.iter().map(|s| s.to_string()).collect(),
            ..TimelineConfig::default()
        }
    }

    #[test]
    fn reads_superseded_date() {
        let tl = cfg(&["【結論・{date}に変更】"]);
        let s = superseded_of(
            &tl,
            "| 10/1 | 【結論・11/20に変更】据え置き |",
            Some(20261123),
        );
        assert_eq!(s.unwrap().date, Some(20261120));
        // 年のない日付は generated.at がなければ読まない（印があることは分かる）
        let s = superseded_of(&tl, "【結論・11/20に変更】据え置き", None);
        assert_eq!(s.unwrap().date, None);
        let s = superseded_of(&tl, "【結論・2026-11-20に変更】", None);
        assert_eq!(s.unwrap().date, Some(20261120));
        assert!(superseded_of(&tl, "【結論】据え置き", None).is_none());
        // 印の後ろの部分がなければ当たらない
        assert!(superseded_of(&tl, "【結論・11/20 据え置き", Some(20261123)).is_none());
        // 前の部分が 2 回出てくる行
        let s = superseded_of(&tl, "【結論・あとで】【結論・2026/12/1に変更】", None);
        assert_eq!(s.unwrap().date, Some(20261201));

        let tl = cfg(&["（変更済み）"]);
        let s = superseded_of(&tl, "価格 （変更済み）", None).unwrap();
        assert_eq!(s.date, None);
        let tl = cfg(&["→{date}"]);
        let s = superseded_of(&tl, "据え置き →2026-12-01 に変更", None).unwrap();
        assert_eq!(s.date, Some(20261201));
        // 前の部分がない印は、後ろの部分の直前の日付を読む
        let tl = cfg(&["{date}に変更"]);
        let s = superseded_of(&tl, "据え置き（12/1に変更）", Some(20261123)).unwrap();
        assert_eq!(s.date, Some(20261201));
        let s = superseded_of(&tl, "据え置き（2026-12-01に変更）", None).unwrap();
        assert_eq!(s.date, Some(20261201));
        let s = superseded_of(&tl, "据え置き（来月に変更）", None).unwrap();
        assert_eq!(s.date, None);
    }

    #[test]
    fn kinds_and_rows() {
        let tl = cfg(&[]);
        assert_eq!(kind_of(&tl, "| 1 | 【結論】値上げ |"), Some("conclusion"));
        assert_eq!(kind_of(&tl, "値上げ"), None);
        assert!(is_delimiter_row("|---|:--:|"));
        assert!(is_delimiter_row(" | --- | --- |"));
        assert!(!is_delimiter_row("| 1 | - |x"));
        assert!(!is_delimiter_row("---"));
    }
}
