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
    /// 1 ページに出す当たった行の数（0 は無制限）。
    pub max_per_page: usize,
    /// 行を出すページの数（0 は無制限）。残りはパスと行数だけを出す。
    pub max_pages: usize,
    /// 当たった行の前後に出す行の数。
    pub context: usize,
    /// 1 行に出す文字数（0 は無制限）。超えた分は省く。
    pub max_columns: usize,
}

impl GrepOptions {
    /// 1 ページに出す行の数の既定値。
    pub const MAX_PER_PAGE: usize = 5;
    /// 行を出すページの数の既定値。
    pub const MAX_PAGES: usize = 30;
    /// 1 行の文字数の既定値。
    pub const MAX_COLUMNS: usize = 500;
}

impl Default for GrepOptions {
    fn default() -> Self {
        GrepOptions {
            no_aliases: false,
            all: false,
            count: false,
            files: false,
            max_per_page: Self::MAX_PER_PAGE,
            max_pages: Self::MAX_PAGES,
            context: 0,
            max_columns: Self::MAX_COLUMNS,
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
    } else {
        print_lines(&r, o, n_terms, out);
    }
    Ok(if r.files.is_empty() { 1 } else { 0 })
}

/// ページの見出し行と当たった行（と前後の行）を出す。上限を超えた分は一覧にする。
fn print_lines(
    r: &search::SearchResult<'_>,
    o: &GrepOptions,
    n_terms: usize,
    out: &mut impl Write,
) {
    let limit = |n: usize| if n == 0 { usize::MAX } else { n };
    let shown_pages = r.files.len().min(limit(o.max_pages));
    let per_page = limit(o.max_per_page);
    let omitted_lines: usize = r.files[..shown_pages]
        .iter()
        .map(|f| f.hits.len().saturating_sub(per_page))
        .sum();
    let rest = &r.files[shown_pages..];
    if omitted_lines > 0 || !rest.is_empty() {
        let _ = writeln!(
            out,
            "# 表示: 上位 {shown_pages} ページ・各 {} 行まで（省いた行 {omitted_lines}、行を出さないページ {}）。順位は 当たった語の種類 → 題名 → 見出し → 行数。すべて出すには --full",
            o.max_per_page,
            rest.len()
        );
    }

    for f in &r.files[..shown_pages] {
        let mut head = format!("# ページ: {}", f.rel);
        let title = f.page.title();
        if !title.is_empty() {
            head.push_str(&format!("「{title}」"));
        }
        head.push_str(&format!(" {} 行", f.hits.len()));
        if n_terms > 1 {
            head.push_str(&format!("・語 {}/{n_terms}", f.terms.count_ones()));
        }
        if f.title_terms != 0 {
            head.push_str("・題名に当たり");
        }
        let _ = writeln!(out, "{head}");

        let shown = &f.hits[..f.hits.len().min(per_page)];
        if o.context == 0 {
            for h in shown {
                let _ = writeln!(out, "{}:{}:{}", f.rel, h.line, clip(h.text, o.max_columns));
            }
        } else {
            print_with_context(f, shown, o, out);
        }
        let hidden = &f.hits[shown.len()..];
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
            let _ = writeln!(
                out,
                "# （このページのほかの {} 行: {}{more}）",
                hidden.len(),
                nums.join(", ")
            );
        }
    }

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
