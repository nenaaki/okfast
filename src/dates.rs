//! 行の日付の取り出し。
//!
//! OKF 0.2 には日付をどこに書くかの決まりがないので、どこから取るかは設定（`[search.recent] date_sources`）で
//! 宣言させる。日付の書式は `YYYY-MM-DD` と `YYYY/MM/DD`（月・日は 1 桁も可）。表のセルに年のない日付
//! （`M-D`・`M/D`）だけが書かれていれば、ページを書いた日（`generated.at`）の年で読む（[`month_day`]）。

use crate::bundle::Page;
use crate::markdown::{self, footnote_def};
use crate::sections::Outline;
use std::collections::HashMap;

/// 日付（`YYYYMMDD` の整数。大小がそのまま新旧になる）。
pub type Date = u32;

/// ページの本文の各行の日付（使い回すため共有する）。
pub type LineDates = std::sync::Arc<[Option<Date>]>;

/// 日付の取り出し元。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DateSource {
    /// 表のデータ行の 1 列目（と、設定で名前を挙げた列）。年のない日付は `generated.at` の年で読む。
    TableRow,
    /// frontmatter のキー（ページの全行に効く）。
    Frontmatter(String),
    /// 行が参照する脚注の本文（出典のパス・題名）。
    FootnoteSource,
    /// 行が属する見出し（祖先を含む）のうち、いちばん近い日付つきのもの。
    Heading,
}

impl DateSource {
    /// 設定の書き方（`table_row`・`frontmatter:<キー>`・`footnote_source`・`heading`）から読む。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "table_row" => Some(DateSource::TableRow),
            "footnote_source" => Some(DateSource::FootnoteSource),
            "heading" => Some(DateSource::Heading),
            _ => s
                .strip_prefix("frontmatter:")
                .map(str::trim)
                .filter(|k| !k.is_empty())
                .map(|k| DateSource::Frontmatter(k.to_string())),
        }
    }
}

/// 文字列の中の日付のうち、最も新しいもの。
pub fn newest_in(s: &str) -> Option<Date> {
    dates_in(s).max()
}

/// 文字列の中の日付（現れた順）。前後が数字のもの、ありえない月日は除く。
pub fn dates_in(s: &str) -> impl Iterator<Item = Date> + '_ {
    let b = s.as_bytes();
    let mut i = 0;
    std::iter::from_fn(move || {
        while i + 8 <= b.len() {
            let at = i;
            i += 1;
            if !b[at].is_ascii_digit() || (at > 0 && b[at - 1].is_ascii_digit()) {
                continue;
            }
            if let Some((d, len)) = date_at(&b[at..]) {
                i = at + len;
                return Some(d);
            }
        }
        None
    })
}

/// 文字列全体が日付なら読む（`--since` 用）。
pub fn parse_exact(s: &str) -> Option<Date> {
    let s = s.trim();
    date_at(s.as_bytes())
        .filter(|&(_, len)| len == s.len())
        .map(|(d, _)| d)
}

/// `YYYY-MM-DD` の形にする。
pub fn format(d: Date) -> String {
    format!("{:04}-{:02}-{:02}", d / 10000, d / 100 % 100, d % 100)
}

/// 先頭から日付を読む。（日付, 読んだバイト数）。後ろに数字が続くものは除く。
fn date_at(b: &[u8]) -> Option<(Date, usize)> {
    let digits = |from: usize, max: usize| -> (u32, usize) {
        let mut v = 0u32;
        let mut n = 0;
        while n < max && from + n < b.len() && b[from + n].is_ascii_digit() {
            v = v * 10 + u32::from(b[from + n] - b'0');
            n += 1;
        }
        (v, n)
    };
    let (year, n) = digits(0, 4);
    if n != 4 {
        return None;
    }
    let sep = *b.get(4)?;
    if sep != b'-' && sep != b'/' {
        return None;
    }
    let (month, mn) = digits(5, 2);
    if mn == 0 || b.get(5 + mn) != Some(&sep) {
        return None;
    }
    let ds = 6 + mn;
    let (day, dn) = digits(ds, 2);
    let end = ds + dn;
    if dn == 0 || b.get(end).is_some_and(u8::is_ascii_digit) {
        return None;
    }
    Some((ymd(year, month, day)?, end))
}

/// 年・月・日を日付にする。ありえない月日は `None`。
fn ymd(year: u32, month: u32, day: u32) -> Option<Date> {
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    };
    if year == 0 || day == 0 || day > days {
        return None;
    }
    Some(year * 10000 + month * 100 + day)
}

/// ページを書いた日（OKF 0.2 §5.2 の `generated.at`）。年のない日付の年を決める基準にする。
pub fn generated_at(page: &Page) -> Option<Date> {
    let at = page.frontmatter().get_path(&["generated", "at"])?;
    let s = match at {
        yaml_rust2::Yaml::String(s) | yaml_rust2::Yaml::Real(s) => s.as_str(),
        _ => return None,
    };
    date_at(s.trim().as_bytes()).map(|(d, _)| d)
}

/// 文字列全体が年のない日付（`M-D`・`M/D`。月・日は 1〜2 桁）なら、`base`（ページを書いた日）の年で読む。
/// 月が `base` の月より 6 か月を超えて先なら、前の年の話とみなす（1 月に書いたページの「12-20」は前年）。
pub fn month_day(s: &str, base: Date) -> Option<Date> {
    let b = s.trim().as_bytes();
    let num = |p: &[u8]| -> Option<u32> {
        (!p.is_empty() && p.len() <= 2 && p.iter().all(u8::is_ascii_digit))
            .then(|| p.iter().fold(0, |v, &c| v * 10 + u32::from(c - b'0')))
    };
    let sep = b.iter().position(|&c| c == b'-' || c == b'/')?;
    let (month, day) = (num(&b[..sep])?, num(&b[sep + 1..])?);
    let mut year = base / 10000;
    if month > base / 100 % 100 + 6 {
        year -= 1;
    }
    ymd(year, month, day)
}

/// ページの本文の各行（0 始まり）の日付。`sources` の順に探し、最初に見つかったものを使う。
///
/// `table_columns` は、`table_row` で 1 列目より先に見る列の名前。
pub fn line_dates(
    page: &Page,
    outline: &Outline,
    sources: &[DateSource],
    table_columns: &[String],
) -> Vec<Option<Date>> {
    let ix = page.text_index();
    let body = page.body();
    let n = ix.lines();
    let mut out: Vec<Option<Date>> = vec![None; n];
    for src in sources {
        if out.iter().all(Option::is_some) {
            break;
        }
        match src {
            DateSource::TableRow => {
                let base = generated_at(page);
                let lines = (0..n).map(|i| (i, ix.line(body, i)));
                for t in markdown::tables(lines) {
                    let cols: Vec<usize> = table_columns
                        .iter()
                        .filter_map(|c| t.column(c))
                        .chain(std::iter::once(0))
                        .collect();
                    for row in &t.rows {
                        if out[row.line].is_some() {
                            continue;
                        }
                        out[row.line] =
                            cols.iter()
                                .filter_map(|&c| row.cells.get(c))
                                .find_map(|cell| {
                                    newest_in(cell)
                                        .or_else(|| base.and_then(|b| month_day(cell, b)))
                                });
                    }
                }
            }
            DateSource::Frontmatter(key) => {
                if let Some(d) = page.frontmatter().string(key).and_then(|v| newest_in(&v)) {
                    out.iter_mut()
                        .filter(|d| d.is_none())
                        .for_each(|x| *x = Some(d));
                }
            }
            DateSource::FootnoteSource => {
                let mut defs: HashMap<&str, Date> = HashMap::new();
                for i in 0..n {
                    if let Some((id, text)) = footnote_def(ix.line(body, i)) {
                        if let Some(d) = newest_in(text) {
                            defs.insert(id, d);
                        }
                    }
                }
                if defs.is_empty() {
                    continue;
                }
                for (i, slot) in out.iter_mut().enumerate() {
                    if slot.is_some() {
                        continue;
                    }
                    let line = ix.line(body, i);
                    *slot = match footnote_def(line) {
                        Some((id, _)) => defs.get(id).copied(),
                        None => footnote_refs(line)
                            .filter_map(|id| defs.get(id))
                            .max()
                            .copied(),
                    };
                }
            }
            DateSource::Heading => {
                let own: Vec<Option<Date>> = outline
                    .headings
                    .iter()
                    .map(|h| newest_in(&h.text))
                    .collect();
                if own.iter().all(Option::is_none) {
                    continue;
                }
                for s in &outline.sections {
                    let Some(h) = s.heading else { continue };
                    let d = std::iter::successors(Some(h), |&k| outline.headings[k].parent)
                        .find_map(|k| own[k]);
                    if d.is_none() {
                        continue;
                    }
                    // 節の終わりの後ろの空行も同じ見出しに属する
                    let next = outline
                        .sections
                        .iter()
                        .find(|x| x.start > s.start)
                        .map_or(n, |x| x.start);
                    out[s.start..next]
                        .iter_mut()
                        .filter(|x| x.is_none())
                        .for_each(|x| *x = d);
                }
            }
        }
    }
    out
}

/// [`line_dates`] を、ページごとに覚えて使い回す（取り方が同じ間だけ）。ページは読み直すと作り直されるので、
/// 本文の変更で古い日付が残ることはない。
pub fn cached_line_dates(
    page: &Page,
    sources: &[DateSource],
    table_columns: &[String],
) -> LineDates {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    sources.hash(&mut h);
    table_columns.hash(&mut h);
    page.cached_dates(h.finish(), || {
        line_dates(page, page.outline(), sources, table_columns)
    })
}

/// 行の中の脚注の参照 `[^id]` の id。
fn footnote_refs(line: &str) -> impl Iterator<Item = &str> {
    let mut rest = line;
    std::iter::from_fn(move || {
        let i = rest.find("[^")?;
        let after = &rest[i + 2..];
        let j = after.find(']')?;
        rest = &after[j + 1..];
        Some(&after[..j])
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bundle::Bundle;

    #[test]
    fn parse_dates() {
        assert_eq!(parse_exact("2026-03-02"), Some(20260302));
        assert_eq!(parse_exact("2026/3/2"), Some(20260302));
        assert_eq!(parse_exact("2026-3/2"), None);
        assert_eq!(parse_exact("2026-02-29"), None);
        assert_eq!(parse_exact("2024-02-29"), Some(20240229));
        assert_eq!(parse_exact("2026-13-01"), None);
        assert_eq!(parse_exact("2026-03"), None);
        assert_eq!(parse_exact("2026-03-02x"), None);
        let found: Vec<Date> =
            dates_in("12026-01-01 と 2026-01-015 と meetings/2026-01-15-kickoff.md、2025/12/1")
                .collect();
        assert_eq!(found, vec![20260115, 20251201]);
        assert_eq!(newest_in("2025-01-01 → 2026-02-03"), Some(20260203));
        assert_eq!(format(20260302), "2026-03-02");
        assert_eq!(
            DateSource::parse("frontmatter:date"),
            Some(DateSource::Frontmatter("date".into()))
        );
        assert_eq!(DateSource::parse("frontmatter:"), None);
        assert_eq!(DateSource::parse("tablerow"), None);
    }

    fn page_dates(text: &str, sources: &[DateSource], cols: &[String]) -> Vec<Option<Date>> {
        let root = std::env::temp_dir().join(format!(
            "okfast-dates-{}-{}",
            std::process::id(),
            text.len()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("p.md"), text).unwrap();
        let b = Bundle::load(&root).unwrap();
        let p = &b.pages[0];
        let o = Outline::build(p.body(), p.text_index());
        let d = line_dates(p, &o, sources, cols);
        let _ = std::fs::remove_dir_all(&root);
        d
    }

    #[test]
    fn sources_in_order() {
        let text = "---\ntype: Log\ndate: 2026-01-01\n---\n\
# 2026-04-01 定例\n\
## 議題\n\
価格を決めた[^1]\n\
| 日 | 内容 | 記録日 |\n\
|---|---|---|\n\
| 2026-03-02 | 改定 | 2026-03-05 |\n\
| — | なし | |\n\
# 別の節\n\
本文\n\
[^1]: meetings/2026-02-10-review.md\n";
        let all = [
            DateSource::TableRow,
            DateSource::FootnoteSource,
            DateSource::Heading,
            DateSource::Frontmatter("date".into()),
        ];
        let d = page_dates(text, &all, &[]);
        assert_eq!(d[0], Some(20260401)); // 見出しそのもの
        assert_eq!(d[1], Some(20260401)); // 祖先の見出し
        assert_eq!(d[2], Some(20260210)); // 脚注が見出しより先
        assert_eq!(d[5], Some(20260302)); // 表の 1 列目
        assert_eq!(d[6], Some(20260401)); // 表の 1 列目が日付でない → 見出し
        assert_eq!(d[8], Some(20260101)); // frontmatter
        assert_eq!(d[9], Some(20260210)); // 脚注の定義そのもの
        let d = page_dates(text, &[DateSource::TableRow], &["記録日".to_string()]);
        assert_eq!(d[5], Some(20260305));
        assert_eq!(d[0], None);
        let d = page_dates(text, &[], &[]);
        assert!(d.iter().all(Option::is_none));
    }
    #[test]
    fn month_day_uses_generated_year() {
        assert_eq!(month_day("04-08", 20261123), Some(20260408));
        assert_eq!(month_day(" 11/20 ", 20261123), Some(20261120));
        assert_eq!(month_day("12-30", 20261123), Some(20261230)); // 先の予定も同じ年
        assert_eq!(month_day("12-20", 20270110), Some(20261220)); // 1 月に書いた 12 月は前年
        assert_eq!(month_day("7/1", 20270110), Some(20270701)); // 6 か月先までは同じ年
        assert_eq!(month_day("8/1", 20270110), Some(20260801));
        assert_eq!(month_day("2-29", 20261123), None);
        assert_eq!(month_day("2-29", 20241123), Some(20240229));
        for s in ["13-01", "1-2-3", "123-4", "4-", "a-1", "1.5", "2026-04-08"] {
            assert_eq!(month_day(s, 20261123), None, "{s}");
        }

        let text = "---\ntype: Log\ngenerated: { by: human:someone, at: 2027-01-10T09:00:00Z }\n---\n\
| 日 | 内容 |\n\
|---|---|\n\
| 12-20 | 前年の話 |\n\
| 01-05 | 今年の話 |\n\
| 2026-03-02 | 年あり |\n\
| 1 月 5 日 | 読まない |\n";
        let d = page_dates(text, &[DateSource::TableRow], &[]);
        assert_eq!(d[2], Some(20261220));
        assert_eq!(d[3], Some(20270105));
        assert_eq!(d[4], Some(20260302));
        assert_eq!(d[5], None);
        // generated.at がなければ年のない日付は読まない
        let d = page_dates(
            "---\ntype: Log\n---\n| 日 | 内容 |\n|---|---|\n| 12-20 | 話 |\n",
            &[DateSource::TableRow],
            &[],
        );
        assert_eq!(d[2], None);
    }

    #[test]
    fn cache_follows_sources() {
        let root = std::env::temp_dir().join(format!("okfast-dates-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("p.md"),
            "---\ntype: Log\ndate: 2026-01-01\n---\n# 2026-04-01 定例\n本文\n",
        )
        .unwrap();
        let b = Bundle::load(&root).unwrap();
        let p = &b.pages[0];
        let h = cached_line_dates(p, &[DateSource::Heading], &[]);
        assert_eq!(h[1], Some(20260401));
        let f = cached_line_dates(p, &[DateSource::Frontmatter("date".into())], &[]);
        assert_eq!(f[1], Some(20260101));
        let again = cached_line_dates(p, &[DateSource::Frontmatter("date".into())], &[]);
        assert!(std::sync::Arc::ptr_eq(&f, &again));
        let _ = std::fs::remove_dir_all(&root);
    }
}
