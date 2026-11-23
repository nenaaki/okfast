//! 同義語の辞書。設定（[`AliasConfig`]）に従ってバンドルから取り出す。
//!
//! - 言い換えは連鎖させない。問いの語が一致した行（概念）の呼び名だけを足す。
//! - 言い換えには必ず出所（ページと行）を付ける。
//! - 取り出しの内訳（[`BuildReport`]）を残し、設定の確かめ（`okfast config check`）に使う。

use crate::bundle::{Bundle, Page};
use crate::config::{AliasConfig, FrontmatterSource, TableSource};
use crate::error::Result;
use crate::markdown;
use crate::normalize::fold;
use crate::selector::Selector;
use std::collections::HashMap;
use std::ops::Range;

/// 言い換えの出所。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    /// ページの相対パス。
    pub page: String,
    /// 行番号（1 始まり）。
    pub line: usize,
}

impl std::fmt::Display for Origin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.page, self.line)
    }
}

/// 条件付きの呼び名（`when` に当たるページを検索するときだけ使う）。
#[derive(Debug, Clone)]
pub struct Conditional {
    /// [`Dictionary::whens`] の番号。
    pub when: usize,
    /// 呼び名。
    pub names: Vec<String>,
}

/// 1 つの概念と、その呼び名。
#[derive(Debug, Clone)]
pub struct AliasGroup {
    /// 概念名（引くためだけに使い、言い換えには足さない）。
    pub concept: Option<String>,
    /// 同じものだと言える呼び名。
    pub names: Vec<String>,
    /// 条件付きの呼び名。
    pub conditional: Vec<Conditional>,
    /// 主題のページ（相対パス）。
    pub boost: Vec<String>,
    /// 出所。
    pub origin: Origin,
}

/// 語の言い換えの結果。
#[derive(Debug, Clone, Default)]
pub struct Expansion {
    /// 足した呼び名（問いの語は含めない）。
    pub added: Vec<String>,
    /// 条件付きで足す呼び名。
    pub conditional: Vec<Conditional>,
    /// 主題のページ。
    pub boost: Vec<String>,
    /// 一致した概念の出所。
    pub origins: Vec<Origin>,
}

impl Expansion {
    /// 何も足さなかったか。
    pub fn is_empty(&self) -> bool {
        self.origins.is_empty()
    }
}

/// `[[aliases.table]]` 1 つ分の取り出しの内訳。
#[derive(Debug, Clone, Default)]
pub struct TableSourceReport {
    /// ページの選び方の説明。
    pub selector: String,
    /// 選び方に当たったページ。
    pub matched: Vec<String>,
    /// trust で除いたページ。
    pub untrusted: Vec<String>,
    /// ページごとの内訳（trust を通ったページ）。
    pub pages: Vec<TablePageReport>,
    /// 作った概念（[`Dictionary::groups`] の範囲）。
    pub groups: Range<usize>,
    /// 条件付きの列ごとの `when`（[`Dictionary::whens`] の番号）。
    pub whens: Vec<usize>,
}

/// 1 ページの表の内訳。
#[derive(Debug, Clone, Default)]
pub struct TablePageReport {
    /// ページ。
    pub page: String,
    /// ページの表の数。
    pub tables: usize,
    /// 必要な列があって使った表の数。
    pub used: usize,
    /// 使わなかった表の見出し行と、その行番号。
    pub unused_headers: Vec<(usize, Vec<String>)>,
    /// 使った表にない条件付きの列。
    pub missing_conditional: Vec<String>,
    /// 使った表に boost の列がなかったか。
    pub missing_boost: bool,
    /// 作った概念の数。
    pub groups: usize,
}

/// `[[aliases.frontmatter]]` 1 つ分の取り出しの内訳。
#[derive(Debug, Clone, Default)]
pub struct FrontmatterSourceReport {
    /// ページの選び方の説明。
    pub selector: String,
    /// 選び方に当たったページの数。
    pub matched: usize,
    /// trust で除いたページ。
    pub untrusted: Vec<String>,
    /// 別名のキーを持っていたページ。
    pub with_keys: Vec<String>,
    /// 作った概念（[`Dictionary::groups`] の範囲）。
    pub groups: Range<usize>,
}

/// 取り出しの内訳。
#[derive(Debug, Clone, Default)]
pub struct BuildReport {
    /// `[[aliases.table]]` ごと。
    pub tables: Vec<TableSourceReport>,
    /// `[[aliases.frontmatter]]` ごと。
    pub frontmatter: Vec<FrontmatterSourceReport>,
}

/// 同義語の辞書。
#[derive(Debug, Default)]
pub struct Dictionary {
    groups: Vec<AliasGroup>,
    whens: Vec<Selector>,
    index: HashMap<String, Vec<usize>>,
    /// 取り出しに使ったページ。
    pub source_pages: Vec<String>,
}

impl Dictionary {
    /// バンドルから辞書を作る。
    pub fn build(bundle: &Bundle, cfg: &AliasConfig) -> Result<Self> {
        Self::build_with_report(bundle, cfg).map(|(d, _)| d)
    }

    /// バンドルから辞書を作り、取り出しの内訳も返す。
    pub fn build_with_report(bundle: &Bundle, cfg: &AliasConfig) -> Result<(Self, BuildReport)> {
        let mut d = Dictionary::default();
        let mut report = BuildReport::default();
        let trusted = |p: &Page| match &cfg.trust {
            None => true,
            Some(t) => p
                .frontmatter()
                .strings(&t.key)
                .iter()
                .any(|v| t.allow.contains(v)),
        };

        for src in &cfg.table {
            let sel = Selector::compile(&src.pages)?;
            let mut r = TableSourceReport {
                selector: sel.description().to_string(),
                ..Default::default()
            };
            for c in &src.conditional {
                d.whens.push(Selector::compile(&c.when)?);
                r.whens.push(d.whens.len() - 1);
            }
            let start = d.groups.len();
            for page in bundle
                .pages
                .iter()
                .filter(|p| sel.matches(&p.rel, p.frontmatter()))
            {
                r.matched.push(page.rel.clone());
                if !trusted(page) {
                    r.untrusted.push(page.rel.clone());
                    continue;
                }
                d.source_pages.push(page.rel.clone());
                let pr = d.add_tables(page, src, &r.whens);
                r.pages.push(pr);
            }
            r.groups = start..d.groups.len();
            report.tables.push(r);
        }

        for src in &cfg.frontmatter {
            let r = d.add_frontmatter(bundle, src, &trusted)?;
            report.frontmatter.push(r);
        }

        d.source_pages.sort();
        d.source_pages.dedup();
        Ok((d, report))
    }

    fn add_frontmatter(
        &mut self,
        bundle: &Bundle,
        src: &FrontmatterSource,
        trusted: &dyn Fn(&Page) -> bool,
    ) -> Result<FrontmatterSourceReport> {
        let sel = Selector::compile(&src.pages)?;
        let mut r = FrontmatterSourceReport {
            selector: sel.description().to_string(),
            ..Default::default()
        };
        let start = self.groups.len();
        for page in bundle
            .pages
            .iter()
            .filter(|p| sel.matches(&p.rel, p.frontmatter()))
        {
            r.matched += 1;
            let mut names: Vec<String> = src
                .keys
                .iter()
                .flat_map(|k| page.frontmatter().strings(k))
                .collect();
            if names.is_empty() {
                continue;
            }
            if !trusted(page) {
                r.untrusted.push(page.rel.clone());
                continue;
            }
            r.with_keys.push(page.rel.clone());
            if src.include_title {
                names.insert(0, page.title());
            }
            dedup(&mut names);
            self.source_pages.push(page.rel.clone());
            self.push(AliasGroup {
                concept: None,
                names,
                conditional: Vec::new(),
                boost: vec![page.rel.clone()],
                origin: Origin {
                    page: page.rel.clone(),
                    line: 1,
                },
            });
        }
        r.groups = start..self.groups.len();
        Ok(r)
    }

    fn add_tables(
        &mut self,
        page: &Page,
        src: &TableSource,
        when_ids: &[usize],
    ) -> TablePageReport {
        // セル → 語の並び（リンクは文字に、脚注・注記・「—」は除く）
        let words = |cell: &str| -> Vec<String> {
            let mut parts = vec![markdown::cell_text(cell)];
            for sep in &src.split {
                parts = parts
                    .iter()
                    .flat_map(|p| {
                        p.split(sep.as_str())
                            .map(str::to_string)
                            .collect::<Vec<_>>()
                    })
                    .collect();
            }
            let mut v: Vec<String> = parts
                .iter()
                .map(|s| markdown::strip_notes(s.trim(), &src.strip_notes))
                .filter(|s| !s.is_empty() && !markdown::is_placeholder(s))
                .collect();
            dedup(&mut v);
            v
        };
        let names_cols_spec = src.names_columns();
        let boost_cols_spec = src.boost_columns();
        let mut r = TablePageReport {
            page: page.rel.clone(),
            ..Default::default()
        };
        let tables = markdown::tables(page.body_lines());
        r.tables = tables.len();
        for table in &tables {
            let names_cols: Option<Vec<usize>> =
                names_cols_spec.iter().map(|c| table.column(c)).collect();
            let concept_col = src.concept.as_ref().map(|c| table.column(c));
            let (Some(names_cols), None | Some(Some(_))) = (names_cols, concept_col) else {
                r.unused_headers
                    .push((table.header_line, table.header.clone()));
                continue;
            };
            let concept_col = concept_col.flatten();
            r.used += 1;
            let boost_cols: Vec<usize> = boost_cols_spec
                .iter()
                .filter_map(|b| table.column(b))
                .collect();
            if boost_cols.len() < boost_cols_spec.len() {
                r.missing_boost = true;
            }
            let mut cond_cols: Vec<(usize, usize)> = Vec::new();
            for (c, &w) in src.conditional.iter().zip(when_ids) {
                match table.column(&c.column) {
                    Some(i) => cond_cols.push((i, w)),
                    None => {
                        if !r.missing_conditional.contains(&c.column) {
                            r.missing_conditional.push(c.column.clone());
                        }
                    }
                }
            }

            for row in &table.rows {
                let cell = |i: usize| row.cells.get(i).map(String::as_str).unwrap_or("");
                let mut names: Vec<String> =
                    names_cols.iter().flat_map(|&i| words(cell(i))).collect();
                dedup(&mut names);
                let concept = concept_col
                    .map(|i| {
                        markdown::strip_notes(markdown::cell_text(cell(i)).trim(), &src.strip_notes)
                    })
                    .filter(|s| !s.is_empty() && !markdown::is_placeholder(s));
                if names.is_empty() && concept.is_none() {
                    continue;
                }
                let conditional = cond_cols
                    .iter()
                    .map(|&(i, w)| Conditional {
                        when: w,
                        names: words(cell(i)),
                    })
                    .filter(|c| !c.names.is_empty())
                    .collect();
                let mut boost: Vec<String> = boost_cols
                    .iter()
                    .flat_map(|&i| markdown::link_targets(cell(i), &page.rel))
                    .collect();
                dedup(&mut boost);
                self.push(AliasGroup {
                    concept,
                    names,
                    conditional,
                    boost,
                    origin: Origin {
                        page: page.rel.clone(),
                        line: row.line,
                    },
                });
                r.groups += 1;
            }
        }
        r
    }

    fn push(&mut self, g: AliasGroup) {
        let id = self.groups.len();
        let mut keys: Vec<String> = g.concept.iter().chain(&g.names).map(|s| fold(s)).collect();
        keys.sort();
        keys.dedup();
        for k in keys.into_iter().filter(|k| !k.is_empty()) {
            self.index.entry(k).or_default().push(id);
        }
        self.groups.push(g);
    }

    /// 概念の数。
    pub fn len(&self) -> usize {
        self.groups.len()
    }

    /// 空か。
    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }

    /// 概念の一覧。
    pub fn groups(&self) -> &[AliasGroup] {
        &self.groups
    }

    /// 条件付きの呼び名の条件。
    pub fn whens(&self) -> &[Selector] {
        &self.whens
    }

    /// 正規化した語ごとの、その語を持つ概念（[`Self::groups`] の番号）。
    pub fn index(&self) -> &HashMap<String, Vec<usize>> {
        &self.index
    }

    /// 語（`alts` はどれかに当たればよい言い換えの並び）を言い換える。連鎖はしない。
    pub fn expand(&self, alts: &[String]) -> Expansion {
        let mut e = Expansion::default();
        let mut seen_groups = Vec::new();
        for alt in alts {
            let Some(ids) = self.index.get(&fold(alt)) else {
                continue;
            };
            for &id in ids {
                if seen_groups.contains(&id) {
                    continue;
                }
                seen_groups.push(id);
                let g = &self.groups[id];
                for n in &g.names {
                    if !alts.contains(n) && !e.added.contains(n) {
                        e.added.push(n.clone());
                    }
                }
                for c in &g.conditional {
                    match e.conditional.iter_mut().find(|x| x.when == c.when) {
                        Some(x) => {
                            for n in &c.names {
                                if !x.names.contains(n) {
                                    x.names.push(n.clone());
                                }
                            }
                        }
                        None => e.conditional.push(c.clone()),
                    }
                }
                for b in &g.boost {
                    if !e.boost.contains(b) {
                        e.boost.push(b.clone());
                    }
                }
                e.origins.push(g.origin.clone());
            }
        }
        e
    }
}

fn dedup(v: &mut Vec<String>) {
    let mut seen = std::collections::HashSet::new();
    v.retain(|s| seen.insert(s.clone()));
}
