//! 設定（okfast.toml）の確かめ。`okfast config check` の中身。
//!
//! 設定が文法として正しくても、当たるページがない・列名が違う、といった誤りは黙って辞書が空になるだけになる。
//! 取り出しの内訳から、何が当たって何が当たらなかったかと、直し方の手がかりを返す。

use crate::aliases::{BuildReport, Dictionary};
use crate::bundle::Bundle;
use crate::config::Config;
use crate::dates;
use crate::error::Result;
use crate::markdown;
use crate::normalize::{fold, is_short};
use crate::selector::compile_glob;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

/// 所見の重さ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// 情報。
    Info,
    /// 設定で直すもの（`config check` の終了コードを 1 にする）。
    Warn,
    /// 辞書の中身について、利用者に確かめるもの（意図どおりのこともある。終了コードには影響しない）。
    Content,
}

/// 1 つの所見。
#[derive(Debug, Clone)]
pub struct Finding {
    /// 重さ。
    pub level: Level,
    /// 内容。
    pub message: String,
}

/// 設定の節ごとの所見。
#[derive(Debug, Clone)]
pub struct Section {
    /// 節の見出し（例: `[[aliases.table]] #1`）。
    pub title: String,
    /// 所見。
    pub findings: Vec<Finding>,
}

/// 確かめの結果。
#[derive(Debug, Clone, Default)]
pub struct Report {
    /// 節ごとの所見。
    pub sections: Vec<Section>,
}

impl Report {
    /// 設定で直す警告の数。
    pub fn warnings(&self) -> usize {
        self.count(Level::Warn)
    }

    /// 辞書の中身についての所見の数。
    pub fn content_notes(&self) -> usize {
        self.count(Level::Content)
    }

    fn count(&self, level: Level) -> usize {
        self.sections
            .iter()
            .flat_map(|s| &s.findings)
            .filter(|f| f.level == level)
            .count()
    }
}

struct SectionBuilder(Section);

impl SectionBuilder {
    fn new(title: impl Into<String>) -> Self {
        SectionBuilder(Section {
            title: title.into(),
            findings: Vec::new(),
        })
    }
    fn info(&mut self, m: impl Into<String>) {
        self.0.findings.push(Finding {
            level: Level::Info,
            message: m.into(),
        });
    }
    fn content(&mut self, m: impl Into<String>) {
        self.0.findings.push(Finding {
            level: Level::Content,
            message: m.into(),
        });
    }
    fn warn(&mut self, m: impl Into<String>) {
        self.0.findings.push(Finding {
            level: Level::Warn,
            message: m.into(),
        });
    }
}

const LIST_MAX: usize = 5;
/// これより長い呼び名は警告する（文字数）。
const LONG_NAME: usize = 20;

fn list<T: AsRef<str>>(items: &[T]) -> String {
    let mut s = items
        .iter()
        .take(LIST_MAX)
        .map(|x| x.as_ref().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    if items.len() > LIST_MAX {
        s.push_str(&format!(" ほか {} 件", items.len() - LIST_MAX));
    }
    s
}

/// 設定をバンドルに当てて確かめる。
pub fn check(bundle: &Bundle, config: &Config, config_path: Option<&Path>) -> Result<Report> {
    let mut report = Report::default();
    let (dict, build) = Dictionary::build_with_report(bundle, &config.aliases)?;

    let mut head = SectionBuilder::new("設定");
    match config_path {
        Some(p) => head.info(format!("設定ファイル: {}", p.display())),
        None => head.warn(
            "設定ファイルがない（<bundle>/okfast.toml も --config もない）。`okfast config init` で下書きを作れる",
        ),
    }
    head.info(format!("バンドル: {} ページ", bundle.pages.len()));
    if config.aliases.table.is_empty() && config.aliases.frontmatter.is_empty() {
        head.warn("同義語の取り出し元（[[aliases.table]] か [[aliases.frontmatter]]）がない。言い換えはしない");
    }
    report.sections.push(head.0);

    check_tables(bundle, config, &dict, &build, &mut report);
    check_frontmatter(bundle, config, &build, &mut report);
    check_dictionary(&dict, config, &mut report);
    check_search(bundle, config, &mut report)?;
    check_search_ranges(bundle, config, &mut report)?;
    Ok(report)
}

fn check_tables(
    bundle: &Bundle,
    config: &Config,
    dict: &Dictionary,
    build: &BuildReport,
    report: &mut Report,
) {
    for (i, (src, r)) in config.aliases.table.iter().zip(&build.tables).enumerate() {
        let mut s = SectionBuilder::new(format!(
            "[[aliases.table]] #{}  pages = {}",
            i + 1,
            r.selector
        ));
        let needed = src.required_columns();

        if r.matched.is_empty() {
            s.warn("当たるページがない");
            let hint = pages_with_columns(bundle, &needed);
            if hint.is_empty() {
                s.info(format!(
                    "列「{}」を持つ表は、バンドルのどこにもない",
                    needed.join("」と「")
                ));
                for col in &needed {
                    let near = similar_headers(bundle, col);
                    if !near.is_empty() {
                        s.info(format!("  「{col}」に似た見出し: {}", list(&near)));
                    }
                }
            } else {
                s.info(format!(
                    "列「{}」を持つ表があるページ: {}",
                    needed.join("」と「"),
                    list(&hint)
                ));
            }
        } else {
            s.info(format!(
                "当たったページ: {}（{}）",
                r.matched.len(),
                list(&r.matched)
            ));
        }
        if !r.untrusted.is_empty() {
            let key = config.aliases.trust.as_ref().map_or("", |t| t.key.as_str());
            s.warn(format!(
                "[aliases.trust] で除いたページ（{key} が許す値でない、またはキーがない）: {}",
                list(&r.untrusted)
            ));
        }

        for p in &r.pages {
            if p.used == 0 {
                if p.tables == 0 {
                    s.warn(format!("{}: 表がない", p.page));
                } else {
                    s.warn(format!(
                        "{}: 列「{}」を持つ表がない（表 {} 個）",
                        p.page,
                        needed.join("」と「"),
                        p.tables
                    ));
                    let mut seen: Vec<&Vec<String>> = Vec::new();
                    for (line, h) in &p.unused_headers {
                        if !seen.contains(&h) {
                            seen.push(h);
                            s.info(format!("  行 {line} の表の見出し: {}", h.join(" | ")));
                        }
                        if seen.len() >= 3 {
                            break;
                        }
                    }
                }
                continue;
            }
            s.info(format!(
                "{}: 表 {}/{} 個を使用、概念 {}",
                p.page, p.used, p.tables, p.groups
            ));
            for c in &p.missing_conditional {
                s.warn(format!("{}: 条件付きの列「{c}」が、使った表にない", p.page));
            }
            if p.missing_boost {
                s.warn(format!(
                    "{}: boost の列「{}」が、使った表にない",
                    p.page,
                    src.boost_columns().join("」「")
                ));
            }
        }

        let groups = &dict.groups()[r.groups.clone()];
        if !groups.is_empty() {
            s.info(format!("概念: {}", groups.len()));
            for g in groups.iter().filter(|g| g.names.len() > 1).take(2) {
                s.info(format!(
                    "  例: {} → {}（{}）",
                    g.concept.as_deref().unwrap_or(&g.names[0]),
                    g.names.join("、"),
                    g.origin
                ));
            }
            let no_names = groups.iter().filter(|g| g.names.is_empty()).count();
            if no_names > 0 {
                s.info(format!(
                    "呼び名の列が空の行: {no_names}（概念名だけでは言い換えない）"
                ));
            }
            let broken: Vec<String> = groups
                .iter()
                .flat_map(|g| {
                    g.boost
                        .iter()
                        .filter(|b| bundle.page(b).is_none())
                        .map(move |b| format!("{b}（行 {}）", g.origin.line))
                })
                .collect();
            if !broken.is_empty() {
                s.content(format!(
                    "主題のページのリンク切れ {}: {}",
                    broken.len(),
                    list(&broken)
                ));
            }
        }

        for (c, &w) in src.conditional.iter().zip(&r.whens) {
            let sel = &dict.whens()[w];
            let n = bundle
                .pages
                .iter()
                .filter(|p| sel.matches(&p.rel, p.frontmatter()))
                .count();
            if n == 0 {
                s.warn(format!(
                    "条件付きの列「{}」の when（{}）に当たるページがない。この列は使われない",
                    c.column,
                    sel.description()
                ));
            } else {
                s.info(format!(
                    "条件付きの列「{}」は {} ページ（{}）でだけ使う",
                    c.column,
                    n,
                    sel.description()
                ));
            }
        }
        report.sections.push(s.0);
    }
}

/// 表の見出しのうち、`col` に似たもの（正規化して一方が他方を含むか、編集距離 2 以下）。
fn similar_headers(bundle: &Bundle, col: &str) -> Vec<String> {
    let want = fold(col);
    let mut out: Vec<String> = Vec::new();
    for p in &bundle.pages {
        for t in markdown::tables(p.body_lines()) {
            for h in &t.header {
                let f = fold(h);
                let near = !f.is_empty()
                    && f != want
                    && (f.contains(&want) || want.contains(&f) || edit_distance(&f, &want) <= 2);
                if near && !out.contains(h) {
                    out.push(h.clone());
                }
            }
        }
    }
    out
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

fn pages_with_columns(bundle: &Bundle, cols: &[String]) -> Vec<String> {
    bundle
        .pages
        .iter()
        .filter(|p| {
            markdown::tables(p.body_lines())
                .iter()
                .any(|t| cols.iter().all(|c| t.column(c).is_some()))
        })
        .map(|p| p.rel.clone())
        .collect()
}

fn check_frontmatter(bundle: &Bundle, config: &Config, build: &BuildReport, report: &mut Report) {
    for (i, (src, r)) in config
        .aliases
        .frontmatter
        .iter()
        .zip(&build.frontmatter)
        .enumerate()
    {
        let mut s = SectionBuilder::new(format!(
            "[[aliases.frontmatter]] #{}  keys = [{}]  pages = {}",
            i + 1,
            src.keys.join(", "),
            r.selector
        ));
        s.info(format!("選び方に当たったページ: {}", r.matched));
        if r.with_keys.is_empty() && r.untrusted.is_empty() {
            s.warn(format!("キー {} を持つページがない", src.keys.join("・")));
            let mut counts: BTreeMap<String, usize> = BTreeMap::new();
            for p in &bundle.pages {
                for k in p.frontmatter().keys() {
                    if p.frontmatter().is_string_list(k) {
                        *counts.entry(k.to_string()).or_default() += 1;
                    }
                }
            }
            let mut v: Vec<(String, usize)> = counts.into_iter().collect();
            v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            if !v.is_empty() {
                let shown: Vec<String> = v.iter().map(|(k, n)| format!("{k}({n})")).collect();
                s.info(format!(
                    "値が文字列の配列のキー（ページ数）: {}",
                    list(&shown)
                ));
            }
        } else {
            s.info(format!(
                "キーを持つページ: {}（{}）",
                r.with_keys.len(),
                list(&r.with_keys)
            ));
        }
        if !r.untrusted.is_empty() {
            s.warn(format!(
                "[aliases.trust] で除いたページ: {}",
                list(&r.untrusted)
            ));
        }
        report.sections.push(s.0);
    }
}

fn check_dictionary(dict: &Dictionary, config: &Config, report: &mut Report) {
    if dict.is_empty() && config.aliases.table.is_empty() && config.aliases.frontmatter.is_empty() {
        return;
    }
    let mut s = SectionBuilder::new("辞書全体");
    s.info(format!(
        "概念: {}、取り出し元のページ: {}",
        dict.len(),
        dict.source_pages.len()
    ));
    if dict.is_empty() {
        s.warn("辞書が空。上の節の警告を直す");
    }

    // 同じ語が複数の概念にあると、言い換えが両方に広がる
    let mut ambiguous: Vec<String> = dict
        .index()
        .iter()
        .filter(|(_, ids)| ids.len() > 1)
        .map(|(key, ids)| {
            let g = &dict.groups()[ids[0]];
            let name = g
                .names
                .iter()
                .chain(&g.concept)
                .find(|n| fold(n) == *key)
                .cloned()
                .unwrap_or_else(|| key.clone());
            let mut at: Vec<String> = ids
                .iter()
                .take(3)
                .map(|&i| dict.groups()[i].origin.to_string())
                .collect();
            if ids.len() > 3 {
                at.push(format!("ほか {} 行", ids.len() - 3));
            }
            format!("「{name}」({})", at.join(" と "))
        })
        .collect();
    ambiguous.sort();
    if !ambiguous.is_empty() {
        s.content(format!(
            "複数の概念にある語 {}（言い換えがすべての概念に広がる。同じものを別の行に書いているだけなら問題ない。別のものなら辞書を直す）: {}",
            ambiguous.len(),
            list(&ambiguous)
        ));
    }

    // 説明文が呼び名の列に入っていると、検索でまず当たらない語になる
    let mut long: Vec<String> = dict
        .groups()
        .iter()
        .flat_map(|g| g.names.iter().map(move |n| (n, &g.origin)))
        .filter(|(n, _)| n.chars().count() > LONG_NAME)
        .map(|(n, o)| {
            let head: String = n.chars().take(LONG_NAME).collect();
            format!("「{head}…」({o})")
        })
        .collect();
    long.dedup();
    if !long.is_empty() {
        s.content(format!(
            "{LONG_NAME} 文字を超える呼び名 {}（説明文や注記が入っていないか。括弧の注記なら設定の strip_notes で外せる）: {}",
            long.len(),
            list(&long)
        ));
    }

    let short_max = config.aliases.matching.short_max;
    let mut short: Vec<String> = dict
        .groups()
        .iter()
        .flat_map(|g| &g.names)
        .filter(|n| is_short(n, short_max))
        .cloned()
        .collect();
    short.sort();
    short.dedup();
    if !short.is_empty() {
        s.info(format!(
            "短い語 {}（{short_max} 文字以下。大文字小文字を区別し単語として当てる。ほかの意味でも当たりやすい）: {}",
            short.len(),
            list(&short)
        ));
    }
    report.sections.push(s.0);
}

fn check_search(bundle: &Bundle, config: &Config, report: &mut Report) -> Result<()> {
    if config.search.exclude.is_empty() {
        return Ok(());
    }
    let mut s = SectionBuilder::new("[search]");
    let mut total: HashMap<&str, ()> = HashMap::new();
    for pat in &config.search.exclude {
        let g = compile_glob(pat)?.compile_matcher();
        let hits: Vec<&str> = bundle
            .pages
            .iter()
            .filter(|p| g.is_match(&p.rel))
            .map(|p| p.rel.as_str())
            .collect();
        if hits.is_empty() {
            s.warn(format!("exclude「{pat}」に当たるページがない"));
        } else {
            s.info(format!("exclude「{pat}」: {} ページ", hits.len()));
        }
        for h in hits {
            total.insert(h, ());
        }
    }
    s.info(format!(
        "既定の検索対象: {} ページ（除外 {}。--all で含める）",
        bundle.pages.len() - total.len(),
        total.len()
    ));
    report.sections.push(s.0);
    Ok(())
}

/// `okfast search` の設定（show_keys・topic_keys・[search.recent]・[search.must_read]）を確かめる。
fn check_search_ranges(bundle: &Bundle, config: &Config, report: &mut Report) -> Result<()> {
    let sc = &config.search;
    let recent = &sc.recent;
    let must = &sc.must_read;

    {
        let mut s = SectionBuilder::new(format!(
            "[search] topic_keys = [{}]",
            sc.topic_keys.join(", ")
        ));
        if sc.topic_keys.is_empty() {
            s.info("主題のページは辞書の boost だけで決める");
        }
        for k in &sc.topic_keys {
            let n = bundle
                .pages
                .iter()
                .filter(|p| !p.frontmatter().strings(k).is_empty())
                .count();
            if n == 0 && k != "title" {
                s.warn(format!("キー {k} を持つページがない"));
            } else if k == "title" {
                s.info(format!(
                    "キー title を持つページ: {n}（ないページはファイル名を使う）"
                ));
            } else {
                s.info(format!("キー {k} を持つページ: {n}"));
            }
        }
        report.sections.push(s.0);
    }

    if !sc.show_keys.is_empty() {
        let mut s = SectionBuilder::new("[search] show_keys");
        for k in &sc.show_keys {
            let n = bundle
                .pages
                .iter()
                .filter(|p| p.frontmatter().string(k).is_some())
                .count();
            if n == 0 {
                s.warn(format!("キー {k} を持つページがない"));
            } else {
                s.info(format!("キー {k} を持つページ: {n}"));
            }
        }
        report.sections.push(s.0);
    }

    if !recent.date_sources.is_empty() || !recent.table_columns.is_empty() {
        let mut s = SectionBuilder::new(format!(
            "[search.recent] date_sources = [{}]",
            recent.date_sources.join(", ")
        ));
        if recent.date_sources.is_empty() {
            s.warn("table_columns があるが date_sources がない。日付は使わない");
        }
        let mut any = false;
        for (name, src) in recent.date_sources.iter().zip(recent.sources()) {
            let (mut lines, mut pages, mut example) = (0usize, 0usize, None);
            for p in &bundle.pages {
                let d = dates::line_dates(
                    p,
                    p.outline(),
                    std::slice::from_ref(&src),
                    &recent.table_columns,
                );
                let n = d.iter().flatten().count();
                if n > 0 {
                    lines += n;
                    pages += 1;
                    if example.is_none() {
                        let i = d.iter().position(Option::is_some).unwrap_or(0);
                        example = Some(format!(
                            "{}:{} → {}",
                            p.rel,
                            p.body_line() + i,
                            dates::format(d[i].unwrap_or_default())
                        ));
                    }
                }
            }
            if lines == 0 {
                s.warn(format!("{name}: 日付の取れた行がない"));
            } else {
                any = true;
                s.info(format!(
                    "{name}: 日付の取れた行 {lines}（{pages} ページ。例: {}）",
                    example.unwrap_or_default()
                ));
            }
        }
        if !any && !recent.date_sources.is_empty() {
            s.info("`okfast config init` が、バンドルにある日付の書き方を提案する");
        }
        if recent.sources().contains(&dates::DateSource::TableRow) {
            // 年のない日付（M-D・M/D）は generated.at の年で読む。generated.at がないページでは読めない
            let mut missing: Vec<String> = Vec::new();
            for p in bundle
                .pages
                .iter()
                .filter(|p| dates::generated_at(p).is_none())
            {
                for t in markdown::tables(p.body_lines()) {
                    if let Some(r) = t.rows.iter().find(|r| {
                        r.cells.first().is_some_and(|c| {
                            dates::cell_date(c, Some(20000101)).is_some()
                                && dates::cell_date(c, None).is_none()
                        })
                    }) {
                        missing.push(format!("{}:{}", p.rel, r.line));
                        break;
                    }
                }
            }
            if !missing.is_empty() {
                s.warn(format!(
                    "表の 1 列目に年のない日付があるが、generated.at がないので年が分からず読めないページ: {}（{}）",
                    missing.len(),
                    list(&missing)
                ));
            }
        }
        for c in &recent.table_columns {
            let found = bundle.pages.iter().any(|p| {
                markdown::tables(p.body_lines())
                    .iter()
                    .any(|t| t.column(c).is_some())
            });
            if !found {
                s.warn(format!("table_columns の列「{c}」を持つ表がない"));
                let near = similar_headers(bundle, c);
                if !near.is_empty() {
                    s.info(format!("  「{c}」に似た見出し: {}", list(&near)));
                }
            }
        }
        report.sections.push(s.0);
    }

    if !must.headings.is_empty() || !must.pages.is_empty() {
        let mut s = SectionBuilder::new("[search.must_read]");
        if !must.headings.is_empty() {
            let words: Vec<String> = must.headings.iter().map(|w| fold(w)).collect();
            let mut found: Vec<String> = Vec::new();
            for p in &bundle.pages {
                for h in &p.outline().headings {
                    let f = fold(&h.text);
                    if words
                        .iter()
                        .any(|w| !w.is_empty() && f.contains(w.as_str()))
                    {
                        found.push(format!(
                            "{}:{}「{}」",
                            p.rel,
                            p.body_line() + h.line,
                            h.text
                        ));
                    }
                }
            }
            if found.is_empty() {
                s.warn(format!(
                    "見出しに「{}」を含む節が、バンドルのどこにもない",
                    must.headings.join("」「")
                ));
            } else {
                s.info(format!(
                    "見出しが当たる節: {}（{}）。主題のページのものだけを出す",
                    found.len(),
                    list(&found)
                ));
            }
        }
        for mp in &must.pages {
            let g = compile_glob(&mp.path)?.compile_matcher();
            let hits: Vec<&str> = bundle
                .pages
                .iter()
                .filter(|p| g.is_match(&p.rel))
                .map(|p| p.rel.as_str())
                .collect();
            if hits.is_empty() {
                s.warn(format!("pages の path「{}」に当たるページがない", mp.path));
            } else {
                let how = match mp.window {
                    None => String::new(),
                    Some(0) => "。当たった行だけ".to_string(),
                    Some(w) => format!("。当たった行の前後 {w} 行"),
                };
                s.info(format!(
                    "pages の path「{}」: {} ページ（上位 {} 範囲を出す{how}）",
                    mp.path,
                    hits.len(),
                    mp.top
                ));
            }
        }
        report.sections.push(s.0);
    }
    Ok(())
}
