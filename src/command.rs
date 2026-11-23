//! `grep` と `aliases` の実行と出力。CLI と常駐プロセスで共有する。

use crate::aliases::Dictionary;
use crate::bundle::Bundle;
use crate::config::Config;
use crate::error::Result;
use crate::search::{self, Options, Query, Term};
use std::io::Write;

/// 常駐プロセスでも実行できるコマンド。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// 語を検索する。
    Grep {
        /// 検索語。
        terms: Vec<String>,
        /// 指定。
        opts: GrepOptions,
    },
    /// 言い換えを表示する。
    Aliases {
        /// 語。
        terms: Vec<String>,
        /// JSON で出す。
        json: bool,
    },
}

/// grep の出し方。
///
/// 既定では、AI が 1 回の呼び出しで次に読むページを決められるよう、順位の高いページから
/// ページごと・全体の行数を絞って出す。省いた行・ページは、行番号やパスの一覧で示す。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrepOptions {
    /// 辞書で言い換えない。
    pub no_aliases: bool,
    /// exclude を無視する。
    pub all: bool,
    /// ページごとの行数だけを出す。
    pub count: bool,
    /// パスだけを出す。
    pub files: bool,
    /// 当たったページをすべて、1 ページ 1 行（題名・行数・いちばん上の当たり行の抜粋）で出す。
    pub pages: bool,
    /// 結果を 1 つの JSON で出す（順位順の全ページ。行は `max_per_page`・`max_pages`・`max_columns` に従う。
    /// `context`・`max_chars` は使わない）。
    pub json: bool,
    /// 1 ページに出す当たった行の数（0 は無制限）。
    pub max_per_page: usize,
    /// 行を出すページの数（0 は無制限）。残りはパスと行数だけを出す。
    pub max_pages: usize,
    /// 当たった行の前後に出す行の数。
    pub context: usize,
    /// 1 行に出す文字数（0 は無制限）。超えた分は省く。
    pub max_columns: usize,
    /// 行の部分全体の文字数の目安（0 は無制限）。超えたら残りのページは一覧に回す。
    pub max_chars: usize,
}

impl GrepOptions {
    /// 1 ページに出す行の数の既定値。
    pub const MAX_PER_PAGE: usize = 5;
    /// 行を出すページの数の既定値。
    pub const MAX_PAGES: usize = 30;
    /// 1 行の文字数の既定値。
    pub const MAX_COLUMNS: usize = 300;
    /// 全体の文字数の既定値。Claude Code は長い出力をファイルに逃がし、読むのに手間が増えるので、
    /// それより十分小さくする。
    pub const MAX_CHARS: usize = 10_000;
}

impl Default for GrepOptions {
    fn default() -> Self {
        GrepOptions {
            no_aliases: false,
            all: false,
            count: false,
            files: false,
            pages: false,
            json: false,
            max_per_page: Self::MAX_PER_PAGE,
            max_pages: Self::MAX_PAGES,
            context: 0,
            max_columns: Self::MAX_COLUMNS,
            max_chars: Self::MAX_CHARS,
        }
    }
}

/// コマンドを実行するための材料。
#[derive(Debug, Clone, Copy)]
pub struct Context<'a> {
    /// バンドル。
    pub bundle: &'a Bundle,
    /// 辞書。
    pub dict: &'a Dictionary,
    /// 設定。
    pub config: &'a Config,
    /// 設定ファイルがあったか。
    pub has_config: bool,
}

/// 実行して `out` に書き、終了コードを返す（grep は当たりなしで 1）。
pub fn run(cmd: &Command, cx: &Context<'_>, out: &mut impl Write) -> Result<u8> {
    match cmd {
        Command::Aliases { terms, json } => aliases(terms, *json, cx, out),
        Command::Grep { terms, opts } => grep(terms, opts, cx, out),
    }
}

fn aliases(terms: &[String], json: bool, cx: &Context<'_>, out: &mut impl Write) -> Result<u8> {
    let dict = cx.dict;
    let q = Query::parse(terms, Some(dict))?;
    if json {
        for t in &q.terms {
            let e = &t.expansion;
            let cond: Vec<String> = e
                .conditional
                .iter()
                .map(|c| {
                    format!(
                        "{{\"when\":{},\"names\":{}}}",
                        json_str(dict.whens()[c.when].description()),
                        json_arr(&c.names)
                    )
                })
                .collect();
            let origins: Vec<String> = e.origins.iter().map(ToString::to_string).collect();
            let _ = writeln!(
                out,
                "{{\"term\":{},\"added\":{},\"conditional\":[{}],\"boost\":{},\"origins\":{}}}",
                json_str(&t.label),
                json_arr(&e.added),
                cond.join(","),
                json_arr(&e.boost),
                json_arr(&origins)
            );
        }
        return Ok(0);
    }
    if !cx.has_config {
        let _ = writeln!(out, "# 設定ファイルがないため、同義語の辞書は空");
    }
    for t in &q.terms {
        print_expansion(out, t, dict);
        if t.expansion.is_empty() {
            let _ = writeln!(out, "{}: 辞書に一致する概念はない", t.label);
        }
    }
    Ok(0)
}

fn grep(terms: &[String], o: &GrepOptions, cx: &Context<'_>, out: &mut impl Write) -> Result<u8> {
    let dict = cx.dict;
    let q = Query::parse(terms, (!o.no_aliases).then_some(dict))?;
    let opt = Options {
        exclude: cx.config.search.exclude.clone(),
        include_all: o.all,
        short_max: cx.config.aliases.matching.short_max,
    };
    let r = search::search(cx.bundle, dict, &q, &opt)?;
    let n_terms = q.terms.len();
    if o.json {
        print_json(&q, &r, &opt, o, dict, out);
        return Ok(if r.files.is_empty() { 1 } else { 0 });
    }

    let labels: Vec<&str> = q.terms.iter().map(|t| t.label.as_str()).collect();
    let _ = writeln!(out, "# grep: {}", labels.join(" ／ "));
    for t in &q.terms {
        print_expansion(out, t, dict);
    }
    let per = r.lines_per_term(n_terms);
    for (t, n) in q.terms.iter().zip(&per) {
        if *n == 0 {
            let _ = writeln!(out, "# 「{}」に当たる行はない", t.label);
        }
    }
    let boosted: Vec<&str> = r
        .files
        .iter()
        .filter(|f| f.boosted)
        .map(|f| f.rel)
        .collect();
    if !boosted.is_empty() {
        let _ = writeln!(out, "# 主題のページ（先に表示）: {}", boosted.join(", "));
    }
    let mut summary = format!(
        "# 当たり: {} 行 / {} ページ（検索 {} ページ",
        r.total_lines(),
        r.files.len(),
        r.searched
    );
    if r.excluded > 0 {
        summary.push_str(&format!(
            "、除外 {}: {} — --all で含める",
            r.excluded,
            opt.exclude.join(", ")
        ));
    }
    summary.push('）');
    let _ = writeln!(out, "{summary}");

    if o.files {
        for f in &r.files {
            let _ = writeln!(out, "{}", f.rel);
        }
    } else if o.count {
        for f in &r.files {
            let _ = writeln!(out, "{}:{}", f.rel, f.hits.len());
        }
    } else if o.pages {
        print_pages(&r, o, n_terms, out);
    } else {
        print_lines(&r, o, n_terms, out);
    }
    Ok(if r.files.is_empty() { 1 } else { 0 })
}

/// ページの見出し行と当たった行（と前後の行）を出す。上限を超えた分は一覧にする。
///
/// 上限は 1 ページの行数・ページ数・全体の文字数の 3 つ。全体の文字数に達したら、
/// それ以降のページはパスと行数の一覧に回す（1 ページ目は少なくとも 1 行出す）。
fn print_lines(
    r: &search::SearchResult<'_>,
    o: &GrepOptions,
    n_terms: usize,
    out: &mut impl Write,
) {
    let limit = |n: usize| if n == 0 { usize::MAX } else { n };
    let max_pages = limit(o.max_pages);
    let per_page = limit(o.max_per_page);
    let budget = limit(o.max_chars);

    let mut body = String::new();
    let mut used = 0usize;
    let mut shown_pages = 0usize;
    let mut omitted_lines = 0usize;
    for f in r.files.iter().take(max_pages) {
        if shown_pages > 0 && used >= budget {
            break;
        }
        let room = budget.saturating_sub(used);
        let most = f.hits.len().min(per_page);
        // 残りの文字数に収まる行数を探す（多いほど長いので二分探索）
        let fits = |k: usize| page_block(f, &f.hits[..k], o, n_terms).chars().count() <= room;
        let k = if fits(most) {
            most
        } else {
            let (mut lo, mut hi) = (0usize, most);
            while lo < hi {
                let mid = (lo + hi).div_ceil(2);
                if fits(mid) { lo = mid } else { hi = mid - 1 }
            }
            lo
        };
        let k = if shown_pages == 0 { k.max(1) } else { k };
        if k == 0 {
            break;
        }
        let mut block = page_block(f, &f.hits[..k], o, n_terms);
        let hidden = &f.hits[k..];
        if !hidden.is_empty() {
            const LIST: usize = 20;
            let nums: Vec<String> = hidden
                .iter()
                .take(LIST)
                .map(|h| h.line.to_string())
                .collect();
            let more = if hidden.len() > LIST {
                format!(" ほか {}", hidden.len() - LIST)
            } else {
                String::new()
            };
            block.push_str(&format!(
                "# （このページのほかの {} 行: {}{more}）\n",
                hidden.len(),
                nums.join(", ")
            ));
        }
        used += block.chars().count();
        body.push_str(&block);
        shown_pages += 1;
        omitted_lines += hidden.len();
    }

    let rest = &r.files[shown_pages..];
    if omitted_lines > 0 || !rest.is_empty() {
        let _ = writeln!(
            out,
            "# 表示: 上位 {shown_pages} ページ（1 ページ {} 行・全体 {} 文字まで。省いた行 {omitted_lines}、行を出さないページ {}）。順位は 当たった語の種類 → 題名 → 見出し → 行数。続きは行番号で Read する。当たったページをすべて 1 行ずつ見るには --pages",
            if o.max_per_page == 0 {
                "無制限".to_string()
            } else {
                o.max_per_page.to_string()
            },
            if o.max_chars == 0 {
                "無制限".to_string()
            } else {
                o.max_chars.to_string()
            },
            rest.len()
        );
    }
    let _ = out.write_all(body.as_bytes());

    if !rest.is_empty() {
        const LIST: usize = 50;
        let items: Vec<String> = rest
            .iter()
            .take(LIST)
            .map(|f| format!("{} ({})", f.rel, f.hits.len()))
            .collect();
        let more = if rest.len() > LIST {
            format!(" ほか {} ページ", rest.len() - LIST)
        } else {
            String::new()
        };
        let _ = writeln!(
            out,
            "# ほかに当たったページ（順位順・行数）: {}{more}",
            items.join(", ")
        );
    }
}

/// 結果を 1 行の JSON で出す。ページは順位順にすべて並べ、当たった行は上位 `max_pages` ページに
/// 1 ページ `max_per_page` 行まで付ける（`lines` は省く前の行数）。書式は README の「JSON の出力」。
fn print_json(
    q: &Query,
    r: &search::SearchResult<'_>,
    opt: &Options,
    o: &GrepOptions,
    dict: &Dictionary,
    out: &mut impl Write,
) {
    use serde::Serialize;
    use std::borrow::Cow;

    #[derive(Serialize)]
    struct Out<'a> {
        terms: Vec<JTerm<'a>>,
        total_lines: usize,
        total_pages: usize,
        searched: usize,
        excluded: usize,
        exclude: &'a [String],
        pages: Vec<JPage<'a>>,
    }
    #[derive(Serialize)]
    struct JTerm<'a> {
        term: &'a str,
        added: &'a [String],
        conditional: Vec<JCond<'a>>,
        boost: &'a [String],
        lines: usize,
    }
    #[derive(Serialize)]
    struct JCond<'a> {
        when: &'a str,
        names: &'a [String],
    }
    #[derive(Serialize)]
    struct JPage<'a> {
        rank: usize,
        path: &'a str,
        title: String,
        lines: usize,
        terms: u32,
        title_hit: bool,
        heading_hits: usize,
        boosted: bool,
        hits: Vec<JHit<'a>>,
    }
    #[derive(Serialize)]
    struct JHit<'a> {
        line: usize,
        text: Cow<'a, str>,
    }

    let limit = |n: usize| if n == 0 { usize::MAX } else { n };
    let per = r.lines_per_term(q.terms.len());
    let terms = q
        .terms
        .iter()
        .zip(&per)
        .map(|(t, &lines)| {
            let e = &t.expansion;
            JTerm {
                term: &t.label,
                added: &e.added,
                conditional: e
                    .conditional
                    .iter()
                    .map(|c| JCond {
                        when: dict.whens()[c.when].description(),
                        names: &c.names,
                    })
                    .collect(),
                boost: &e.boost,
                lines,
            }
        })
        .collect();
    let pages = r
        .files
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let shown = if i < limit(o.max_pages) {
                &f.hits[..f.hits.len().min(limit(o.max_per_page))]
            } else {
                &[]
            };
            JPage {
                rank: i + 1,
                path: f.rel,
                title: f.page.title(),
                lines: f.hits.len(),
                terms: f.terms.count_ones(),
                title_hit: f.title_terms != 0,
                heading_hits: f.heading_hits,
                boosted: f.boosted,
                hits: shown
                    .iter()
                    .map(|h| JHit {
                        line: h.line,
                        text: clip(h.text, o.max_columns),
                    })
                    .collect(),
            }
        })
        .collect();
    let v = Out {
        terms,
        total_lines: r.total_lines(),
        total_pages: r.files.len(),
        searched: r.searched,
        excluded: r.excluded,
        exclude: if r.excluded > 0 { &opt.exclude } else { &[] },
        pages,
    };
    let _ = serde_json::to_writer(&mut *out, &v);
    let _ = writeln!(out);
}

/// 抜粋の文字数。
const SNIPPET: usize = 120;

/// 当たったページをすべて 1 ページ 1 行で出す: `パス「題名」 N 行[・語 a/b]: 行番号: 抜粋`。
/// 全体の文字数の目安を超えたら、残りは抜粋なしの `パス (行数)` の一覧にする（ページは落とさない）。
fn print_pages(
    r: &search::SearchResult<'_>,
    o: &GrepOptions,
    n_terms: usize,
    out: &mut impl Write,
) {
    let budget = if o.max_chars == 0 {
        usize::MAX
    } else {
        o.max_chars
    };
    let mut used = 0usize;
    let mut rest = Vec::new();
    for f in &r.files {
        if used >= budget {
            rest.push(format!("{} ({})", f.rel, f.hits.len()));
            continue;
        }
        let mut line = f.rel.to_string();
        let title = f.page.title();
        if !title.is_empty() {
            line.push_str(&format!("「{title}」"));
        }
        line.push_str(&format!(" {} 行", f.hits.len()));
        if n_terms > 1 {
            line.push_str(&format!("・語 {}/{n_terms}", f.terms.count_ones()));
        }
        if let Some(h) = f.hits.first() {
            line.push_str(&format!(
                ": {}: {}",
                h.line,
                snippet(h.text.trim(), SNIPPET)
            ));
        }
        used += line.chars().count() + 1;
        let _ = writeln!(out, "{line}");
    }
    if !rest.is_empty() {
        let _ = writeln!(
            out,
            "# 残りのページ（抜粋なし・順位順・行数）: {}",
            rest.join(", ")
        );
    }
}

/// 短い抜粋（`max` 文字で切って … を付ける）。
fn snippet(s: &str, max: usize) -> std::borrow::Cow<'_, str> {
    match s.char_indices().nth(max) {
        None => std::borrow::Cow::Borrowed(s),
        Some((cut, _)) => std::borrow::Cow::Owned(format!("{}…", &s[..cut])),
    }
}

/// 1 ページぶんの見出し行と、`shown` の行（と前後の行）。
fn page_block(
    f: &search::FileHits<'_>,
    shown: &[search::LineHit<'_>],
    o: &GrepOptions,
    n_terms: usize,
) -> String {
    let mut s = format!("# ページ: {}", f.rel);
    let title = f.page.title();
    if !title.is_empty() {
        s.push_str(&format!("「{title}」"));
    }
    s.push_str(&format!(" {} 行", f.hits.len()));
    if n_terms > 1 {
        s.push_str(&format!("・語 {}/{n_terms}", f.terms.count_ones()));
    }
    if f.title_terms != 0 {
        s.push_str("・題名に当たり");
    }
    s.push('\n');
    let mut buf = Vec::new();
    if o.context == 0 {
        for h in shown {
            let _ = writeln!(buf, "{}:{}:{}", f.rel, h.line, clip(h.text, o.max_columns));
        }
    } else {
        print_with_context(f, shown, o, &mut buf);
    }
    s.push_str(&String::from_utf8_lossy(&buf));
    s
}

/// 当たった行に前後の行を付けて出す。当たった行は `パス:行:内容`、前後の行は `パス-行-内容`、
/// 離れた塊のあいだは `--`。
fn print_with_context(
    f: &search::FileHits<'_>,
    shown: &[search::LineHit<'_>],
    o: &GrepOptions,
    out: &mut impl Write,
) {
    let page = f.page;
    let ix = page.text_index();
    let body = page.body();
    let first = page.body_line();
    let end = first + ix.lines(); // 本文の最後の行の次
    let mut printed_to = 0usize; // この行番号の手前まで出した
    for (k, h) in shown.iter().enumerate() {
        let from = h.line.saturating_sub(o.context).max(first).max(printed_to);
        let to = (h.line + o.context + 1).min(end);
        if k > 0 && from > printed_to {
            let _ = writeln!(out, "--");
        }
        for no in from..to {
            let text = ix.line(body, no - first);
            let sep = if f.hits.binary_search_by_key(&no, |x| x.line).is_ok() {
                ':'
            } else {
                '-'
            };
            let _ = writeln!(out, "{}{sep}{no}{sep}{}", f.rel, clip(text, o.max_columns));
        }
        printed_to = printed_to.max(to);
    }
}

/// 長すぎる行を `max` 文字で切る（0 は切らない）。
fn clip(s: &str, max: usize) -> std::borrow::Cow<'_, str> {
    if max == 0 || s.len() <= max {
        return std::borrow::Cow::Borrowed(s);
    }
    match s.char_indices().nth(max) {
        None => std::borrow::Cow::Borrowed(s),
        Some((cut, _)) => {
            let total = s.chars().count();
            std::borrow::Cow::Owned(format!(
                "{}…（{total} 文字のうち {max} 文字まで）",
                &s[..cut]
            ))
        }
    }
}

fn print_expansion(out: &mut impl Write, t: &Term, dict: &Dictionary) {
    let e = &t.expansion;
    if e.is_empty() {
        return;
    }
    let origins: Vec<String> = e.origins.iter().map(ToString::to_string).collect();
    let added = if e.added.is_empty() {
        "（足す呼び名なし）".to_string()
    } else {
        e.added.join("、")
    };
    let _ = writeln!(
        out,
        "# 言い換え: {} → {}  ({})",
        t.label,
        added,
        origins.join(", ")
    );
    for c in &e.conditional {
        let when = dict.whens()[c.when].description();
        let _ = writeln!(out, "#   {} のページでだけ: {}", when, c.names.join("、"));
    }
    if !e.boost.is_empty() {
        let _ = writeln!(out, "#   主題のページ: {}", e.boost.join(", "));
    }
}

fn json_str(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn json_arr<'a>(v: impl IntoIterator<Item = &'a String>) -> String {
    let items: Vec<String> = v.into_iter().map(|s| json_str(s)).collect();
    format!("[{}]", items.join(","))
}
