//! 設定の下書きを作る。`okfast config init` の中身。
//!
//! バンドルを走査して、同義語の取り出し元の候補（別名の列を持つ表、別名らしい frontmatter のキー）を探し、
//! 根拠をコメントに書いた okfast.toml の下書きを返す。見つけたものを勝手に有効にはしない
//! （下書きを確かめて置くのは利用者か AI。確かめには `okfast config check` を使う）。
//!
//! 列の名前だけでなく表の中身も見て、書き方を推定する:
//! - 概念名が呼び名の列にも書かれているか（書かれていなければ、概念名の列も呼び名の列にする）
//! - 概念名の列にリンクがあるか（あれば、主題のページに使う）
//! - セルの中の区切り、括弧の注記の有無

use crate::bundle::Bundle;
use crate::dates;
use crate::markdown;
use crate::normalize::fold;
use std::collections::BTreeMap;
use std::fmt::Write;

/// 呼び名（同じものだと言える語）の列の名前の候補。
const NAME_COLUMNS: &[&str] = &[
    "呼び名",
    "別名",
    "同義語",
    "言い換え",
    "表記",
    "略称",
    "旧語",
    "旧称",
    "names",
    "aliases",
    "alias",
    "synonyms",
    "synonym",
    "variants",
    "also known as",
    "aka",
    "alternate names",
    "other names",
];
/// 概念名の列の名前の候補。
const CONCEPT_COLUMNS: &[&str] = &[
    "概念",
    "用語",
    "正式名",
    "名称",
    "統一後",
    "今の語",
    "正",
    "concept",
    "term",
    "canonical",
    "name",
    "correct",
];
/// 誤認識・誤記の列の名前の候補。
const MISREAD_COLUMNS: &[&str] = &[
    "誤認識",
    "誤変換",
    "誤記",
    "誤",
    "misheard",
    "misrecognition",
    "misrecognitions",
    "asr errors",
    "wrong",
    "typo",
    "typos",
];
/// 主題のページ（リンク）の列の名前の候補。
const TOPIC_COLUMNS: &[&str] = &[
    "主題のページ",
    "主題",
    "関連ページ",
    "topic",
    "topic page",
    "page",
    "see",
];
/// 未確定の語を並べた表らしい見出しの一部。
const TENTATIVE_HEADERS: &[&str] = &["候補", "未確定", "検討中", "candidate", "proposed"];
/// 別名の frontmatter キーの候補。
const ALIAS_KEYS: &[&str] = &[
    "aliases",
    "alias",
    "also_known_as",
    "aka",
    "synonyms",
    "alternate_names",
    "別名",
];
/// 誤認識の列を使うページ（文字起こしなど）がありそうなディレクトリ名の一部。
const TRANSCRIPT_DIRS: &[&str] = &["transcript", "文字起こし", "recording", "minutes-raw"];
/// セルの中の区切りの候補。
const SEPARATORS: &[&str] = &["、", ",", "，", "／", ";"];

/// 見出しが候補に当たるか。ASCII の候補と 1 文字の候補は完全一致、それ以外は部分一致。
fn column_matches(header: &str, candidates: &[&str]) -> bool {
    let h = fold(header);
    candidates.iter().any(|c| {
        let c = fold(c);
        if c.is_ascii() || c.chars().count() == 1 {
            h == c
        } else {
            h.contains(&c)
        }
    })
}

fn find_column<'a>(headers: &'a [String], candidates: &[&str]) -> Option<&'a String> {
    headers.iter().find(|h| column_matches(h, candidates))
}

fn toml_str(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c if (c as u32) < 0x20 => {
                let _ = write!(o, "\\u{:04X}", c as u32);
            }
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn toml_list(items: &[String]) -> String {
    format!(
        "[{}]",
        items
            .iter()
            .map(|s| toml_str(s))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// 表の列の組み合わせ（下書きの [[aliases.table]] 1 つに当たる）。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Columns {
    /// 呼び名の列（ない表は、概念名と誤記の列だけの表）。
    names: Option<String>,
    concept: Option<String>,
    misread: Option<String>,
    topic: Option<String>,
    tentative: bool,
}

#[derive(Debug, Default)]
struct Found {
    pages: Vec<String>,
    tables: usize,
    rows: usize,
    evidence: Vec<String>,
    /// 概念名と呼び名の両方がある行のうち、概念名が呼び名にも書かれている行。
    concept_in_names: usize,
    concept_and_names: usize,
    /// 概念名の列にリンクがある行。
    concept_links: usize,
    /// 呼び名に括弧の注記がある行。
    notes: usize,
    /// 区切りごとの出現行数。
    seps: BTreeMap<&'static str, usize>,
}

fn words(cell: &str, seps: &[&str]) -> Vec<String> {
    let mut parts = vec![markdown::cell_text(cell)];
    for sep in seps {
        parts = parts
            .iter()
            .flat_map(|p| p.split(sep).map(str::to_string).collect::<Vec<_>>())
            .collect();
    }
    parts
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && !markdown::is_placeholder(s))
        .collect()
}

/// 下書きを作る。
pub fn draft(bundle: &Bundle) -> String {
    let mut tables: BTreeMap<Columns, Found> = BTreeMap::new();
    for page in &bundle.pages {
        for t in markdown::tables(page.body_lines()) {
            let names = find_column(&t.header, NAME_COLUMNS);
            let others: Vec<String> = t
                .header
                .iter()
                .filter(|h| Some(*h) != names)
                .cloned()
                .collect();
            let concept = find_column(&others, CONCEPT_COLUMNS);
            let misread = find_column(&others, MISREAD_COLUMNS);
            // 呼び名の列がなくても、概念名と誤記の列があれば候補にする（「正｜誤」の表など）
            if names.is_none() && !(concept.is_some() && misread.is_some()) {
                continue;
            }
            let cols = Columns {
                names: names.cloned(),
                concept: concept.cloned(),
                misread: misread.cloned(),
                topic: find_column(&others, TOPIC_COLUMNS).cloned(),
                tentative: t
                    .header
                    .iter()
                    .any(|h| column_matches(h, TENTATIVE_HEADERS)),
            };
            let idx = |c: &Option<String>| c.as_ref().and_then(|c| t.column(c));
            let (ni, ci) = (idx(&cols.names), idx(&cols.concept));
            let f = tables.entry(cols).or_default();
            if !f.pages.contains(&page.rel) {
                f.pages.push(page.rel.clone());
            }
            f.tables += 1;
            f.rows += t.rows.len();
            if f.evidence.len() < 3 {
                f.evidence.push(format!(
                    "{}:{}（見出し: {}）",
                    page.rel,
                    t.header_line,
                    t.header.join(" | ")
                ));
            }
            for row in &t.rows {
                let cell =
                    |i: Option<usize>| i.and_then(|i| row.cells.get(i)).map_or("", String::as_str);
                let names_cell = cell(ni);
                let concept_cell = cell(ci);
                for sep in SEPARATORS {
                    if markdown::cell_text(names_cell).contains(sep) {
                        *f.seps.entry(sep).or_default() += 1;
                    }
                }
                if names_cell.contains('（') || names_cell.contains('(') {
                    f.notes += 1;
                }
                if concept_cell.contains("](") {
                    f.concept_links += 1;
                }
                let concept_text = markdown::cell_text(concept_cell).trim().to_string();
                let ws = words(names_cell, SEPARATORS);
                if !concept_text.is_empty() && !ws.is_empty() {
                    f.concept_and_names += 1;
                    if ws.iter().any(|w| fold(w) == fold(&concept_text)) {
                        f.concept_in_names += 1;
                    }
                }
            }
        }
    }

    let mut keys: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for page in &bundle.pages {
        let fm = page.frontmatter();
        for k in ALIAS_KEYS {
            if !fm.strings(k).is_empty() {
                keys.entry(k).or_default().push(page.rel.clone());
            }
        }
    }

    let mut transcript_dirs: Vec<String> = Vec::new();
    for page in &bundle.pages {
        let mut acc = String::new();
        let parts: Vec<&str> = page.rel.split('/').collect();
        for part in &parts[..parts.len().saturating_sub(1)] {
            if !acc.is_empty() {
                acc.push('/');
            }
            acc.push_str(part);
            let lower = part.to_lowercase();
            if TRANSCRIPT_DIRS.iter().any(|d| lower.contains(d)) {
                let glob = format!("{acc}/**");
                if !transcript_dirs.contains(&glob) {
                    transcript_dirs.push(glob);
                }
                break;
            }
        }
    }

    let mut o = String::new();
    let _ = writeln!(
        o,
        "# okfast.toml の下書き（okfast config init が {} ページを走査して作成）",
        bundle.pages.len()
    );
    let _ = writeln!(
        o,
        "# 根拠をコメントに書いた。中身を確かめ、`okfast config check` が警告なしになるまで直してから使う。"
    );
    let _ = writeln!(o, "# 書式: okfast config reference");
    let _ = writeln!(o);

    let mut dict_pages: Vec<String> = Vec::new();
    if tables.is_empty() {
        let _ = writeln!(
            o,
            "# 別名の列（{} など）を持つ表は見つからなかった。",
            NAME_COLUMNS[..5].join("・")
        );
        let _ = writeln!(o, "# 表で同義語を管理しているなら、次を直して有効にする。");
        let _ = writeln!(o, "# [[aliases.table]]");
        let _ = writeln!(o, "# pages = {{ path = \"glossary/別名表.md\" }}");
        let _ = writeln!(o, "# names = \"呼び名\"");
        let _ = writeln!(o);
    }
    for (cols, f) in &tables {
        write_table(&mut o, cols, f, &transcript_dirs);
        if !cols.tentative {
            dict_pages.extend(f.pages.iter().cloned());
        }
    }

    if keys.is_empty() {
        let _ = writeln!(
            o,
            "# 別名らしい frontmatter のキー（{}）を持つページは見つからなかった。",
            ALIAS_KEYS[..3].join("・")
        );
        let _ = writeln!(
            o,
            "# ページごとに別名を書くなら、frontmatter に aliases: [..] を書き、次を有効にする。"
        );
        let _ = writeln!(o, "# [[aliases.frontmatter]]");
        let _ = writeln!(o, "# keys = [\"aliases\"]");
    } else {
        for (k, pages) in &keys {
            let _ = writeln!(
                o,
                "# 根拠: キー {k} を持つページ {}（例: {}）",
                pages.len(),
                pages.iter().take(3).cloned().collect::<Vec<_>>().join(", ")
            );
        }
        let ks: Vec<String> = keys.keys().map(|k| k.to_string()).collect();
        let _ = writeln!(o, "[[aliases.frontmatter]]");
        let _ = writeln!(o, "keys = {}", toml_list(&ks));
    }
    let _ = writeln!(o);

    let _ = writeln!(o, "[aliases.match]");
    let _ = writeln!(
        o,
        "short_max = 3   # この文字数以下の英数字の語は、大文字小文字を区別し単語として当てる"
    );
    let _ = writeln!(o);
    let _ = writeln!(
        o,
        "# 取り出し元のページを frontmatter の値で絞るなら（キーを持たないページは使わなくなる）"
    );
    let _ = writeln!(o, "# [aliases.trust]");
    let _ = writeln!(o, "# key   = \"credibility\"");
    let _ = writeln!(o, "# allow = [\"A\", \"B\"]");
    let _ = writeln!(o);

    let _ = writeln!(o, "[search]");
    dict_pages.sort();
    dict_pages.dedup();
    if !dict_pages.is_empty() {
        let _ = writeln!(
            o,
            "# 辞書のページそのものは、どの語でも当たってしまうので既定で除く（--all で含める）"
        );
    }
    if !transcript_dirs.is_empty() {
        let _ = writeln!(
            o,
            "# 文字起こしも既定で除くなら次を足す: {}",
            toml_list(&transcript_dirs)
        );
    }
    let _ = writeln!(o, "exclude = {}", toml_list(&dict_pages));
    write_search_ranges(&mut o, bundle);
    o
}

fn write_table(o: &mut String, cols: &Columns, f: &Found, transcript_dirs: &[String]) {
    let _ = writeln!(o, "# 根拠: 表 {} 個・{} 行。例:", f.tables, f.rows);
    for e in &f.evidence {
        let _ = writeln!(o, "#   {e}");
    }

    // 概念名が呼び名の列にも書かれていないなら、概念名の列も呼び名の列にする
    let concept_is_separate = match (&cols.names, &cols.concept) {
        (Some(_), Some(_)) => {
            f.concept_and_names > 0 && f.concept_in_names * 2 > f.concept_and_names
        }
        _ => false,
    };
    let mut names: Vec<String> = Vec::new();
    let mut concept: Option<&String> = None;
    match (&cols.names, &cols.concept) {
        (Some(n), Some(c)) if concept_is_separate => {
            names.push(n.clone());
            concept = Some(c);
        }
        (Some(n), Some(c)) => {
            names.push(c.clone());
            names.push(n.clone());
        }
        (Some(n), None) => names.push(n.clone()),
        (None, Some(c)) => names.push(c.clone()),
        (None, None) => {}
    }

    // 未確定の表は有効にしない（同じものだと確定した語だけを使う）
    let p = if cols.tentative { "# " } else { "" };
    if cols.tentative {
        let _ = writeln!(
            o,
            "# 見出しから、統一が決まっていない語の表に見える。確定した語だけを同義語にするなら使わない"
        );
    }
    if cols.names.is_none() {
        let _ = writeln!(
            o,
            "# 呼び名の列がなく、概念名と誤記の列だけの表。誤記は条件付きの列として使う"
        );
    }
    let _ = writeln!(o, "{p}[[aliases.table]]");
    let path = if f.pages.len() == 1 {
        toml_str(&f.pages[0])
    } else {
        toml_list(&f.pages)
    };
    let _ = writeln!(o, "{p}pages   = {{ path = {path} }}");
    match concept {
        Some(c) => {
            let _ = writeln!(
                o,
                "{p}concept = {}   # 概念名が呼び名の列にも書かれている（{}/{} 行）",
                toml_str(c),
                f.concept_in_names,
                f.concept_and_names
            );
        }
        None if cols.names.is_some() && cols.concept.is_some() => {
            let _ = writeln!(
                o,
                "# 概念名が呼び名の列に書かれていない行が多い（書かれているのは {}/{} 行）ので、概念名の列も呼び名にする",
                f.concept_in_names, f.concept_and_names
            );
        }
        None => {}
    }
    if names.len() == 1 {
        let _ = writeln!(o, "{p}names   = {}", toml_str(&names[0]));
    } else {
        let _ = writeln!(o, "{p}names   = {}", toml_list(&names));
    }
    let mut seps: Vec<String> = f.seps.keys().map(|s| s.to_string()).collect();
    if seps.is_empty() {
        seps.push("、".to_string());
    }
    let _ = writeln!(
        o,
        "{p}split   = {}   # セルの中で使われていた区切り",
        toml_list(&seps)
    );
    if f.notes > 0 {
        let _ = writeln!(
            o,
            "# 呼び名に括弧のある行が {} ある。注記なら外す（括弧まで含めて名前なら外さない）",
            f.notes
        );
        let _ = writeln!(o, "# strip_notes = [\"（\", \"(\"]");
    }
    match (
        &cols.topic,
        f.concept_links > 0,
        concept.or(cols.concept.as_ref()),
    ) {
        (Some(t), _, _) => {
            let _ = writeln!(
                o,
                "{p}boost   = {}   # この列のリンク先を検索結果の上位に出す",
                toml_str(t)
            );
        }
        (None, true, Some(c)) => {
            let _ = writeln!(
                o,
                "{p}boost   = {}   # 概念名の列にリンクがある（{} 行）。リンク先を検索結果の上位に出す",
                toml_str(c),
                f.concept_links
            );
        }
        _ => {}
    }
    if let Some(m) = &cols.misread {
        let _ = writeln!(o);
        match transcript_dirs.first() {
            Some(d) => {
                let _ = writeln!(
                    o,
                    "{p}# 誤記の列は、文字起こしらしいページを検索するときだけ使う"
                );
                let _ = writeln!(o, "{p}[[aliases.table.conditional]]");
                let _ = writeln!(o, "{p}column = {}", toml_str(m));
                let _ = writeln!(o, "{p}when   = {{ path = {} }}", toml_str(d));
            }
            None => {
                let _ = writeln!(
                    o,
                    "# 誤記の列がある。誤記が残っていそうなページ（文字起こし・議事録など）を when に書いて有効にする"
                );
                let _ = writeln!(o, "# [[aliases.table.conditional]]");
                let _ = writeln!(o, "# column = {}", toml_str(m));
                let _ = writeln!(
                    o,
                    "# when   = {{ path = \"<文字起こしのディレクトリ>/**\" }}"
                );
            }
        }
    }
    let _ = writeln!(o);
}

/// `okfast search` の設定の下書き。日付の書き方はバンドルを走査して、見つかったものだけを提案する。
fn write_search_ranges(o: &mut String, bundle: &Bundle) {
    let mut table_rows = (0usize, None::<String>);
    let mut fm: BTreeMap<String, (usize, String)> = BTreeMap::new();
    let mut footnotes = (0usize, None::<String>);
    let mut headings = (0usize, None::<String>);
    let note = |slot: &mut (usize, Option<String>), at: String| {
        slot.0 += 1;
        slot.1.get_or_insert(at);
    };
    let mut described = (0usize, 0usize);
    for p in &bundle.pages {
        let first = p.body_line();
        let fmv = p.frontmatter();
        described.0 += usize::from(!fmv.strings("description").is_empty());
        described.1 += usize::from(!fmv.strings("tags").is_empty());
        // 年のない日付（M-D・M/D）は generated.at の年で読む
        let generated = dates::generated_at(p);
        for t in markdown::tables(p.body_lines()) {
            for r in &t.rows {
                if r.cells
                    .first()
                    .is_some_and(|c| dates::cell_date(c, generated).is_some())
                {
                    note(&mut table_rows, format!("{}:{}", p.rel, r.line));
                }
            }
        }
        for k in p.frontmatter().keys() {
            if p.frontmatter()
                .string(k)
                .is_some_and(|v| dates::parse_exact(&v).is_some())
            {
                let e = fm.entry(k.to_string()).or_insert((0, p.rel.clone()));
                e.0 += 1;
            }
        }
        for (i, line) in p.body().lines().enumerate() {
            let t = line.trim_start();
            if t.starts_with("[^") && t.contains("]:") && dates::newest_in(t).is_some() {
                note(&mut footnotes, format!("{}:{}", p.rel, first + i));
            }
        }
        for h in &p.outline().headings {
            if dates::newest_in(&h.text).is_some() {
                note(&mut headings, format!("{}:{}", p.rel, first + h.line));
            }
        }
    }

    let _ = writeln!(o);
    let _ = writeln!(
        o,
        "# okfast search（読む範囲を返す検索）の設定。既定値のままでよければ書かなくてよい"
    );
    let n = bundle.pages.len();
    let mut keys = vec!["title".to_string()];
    if described.0 * 2 >= n && described.0 > 0 {
        keys.push("description".into());
    }
    if described.1 * 2 >= n && described.1 > 0 {
        keys.push("tags".into());
    }
    let _ = writeln!(
        o,
        "# 主題のページを決める frontmatter のキー（値に語が当たったページの範囲は点が上がる）"
    );
    if keys.len() > 1 {
        let _ = writeln!(
            o,
            "# 根拠: description を持つページ {}・tags を持つページ {}（全 {n}）",
            described.0, described.1
        );
        let _ = writeln!(o, "topic_keys = {}", toml_list(&keys));
    } else {
        let _ = writeln!(o, "# topic_keys = [\"title\"]   # 既定");
    }
    let _ = writeln!(o);
    let _ = writeln!(o, "# [search.sections]");
    let _ = writeln!(
        o,
        "# max_lines = 40   # これより長い節は、当たった行の前後だけを範囲にする"
    );
    let _ = writeln!(o, "# window    = 4    # その前後の行数");
    let _ = writeln!(
        o,
        "# score     = \"decay\"  # 範囲の点: decay（当たった行の点を 1・1/2・1/4… で足す）か max（語ごとの最高点）"
    );
    let _ = writeln!(
        o,
        "# skip_footnotes = true  # 脚注の定義の行（[^id]: 出典）を当たりに数えない"
    );
    let _ = writeln!(
        o,
        "# tf = \"count\"   # 行の中の語の出現回数を数える（binary なら 1 行 1 回）。k1 = 1.2・b = 0.5"
    );
    let _ = writeln!(o);

    let mut sources: Vec<String> = Vec::new();
    let mut evidence: Vec<String> = Vec::new();
    if table_rows.0 > 0 {
        sources.push("table_row".into());
        evidence.push(format!(
            "表の 1 列目が日付の行 {}（例: {}）",
            table_rows.0,
            table_rows.1.unwrap_or_default()
        ));
    }
    for (k, (n, example)) in &fm {
        sources.push(format!("frontmatter:{k}"));
        evidence.push(format!(
            "frontmatter の {k} が日付のページ {n}（例: {example}）"
        ));
    }
    if footnotes.0 > 0 {
        sources.push("footnote_source".into());
        evidence.push(format!(
            "日付を含む脚注の定義 {}（例: {}）",
            footnotes.0,
            footnotes.1.unwrap_or_default()
        ));
    }
    if headings.0 > 0 {
        sources.push("heading".into());
        evidence.push(format!(
            "日付を含む見出し {}（例: {}）",
            headings.0,
            headings.1.unwrap_or_default()
        ));
    }
    let _ = writeln!(
        o,
        "# 行の日付の取り方。新しい記録を足す・--since・--sort date に使う。先に書いたものから探す"
    );
    if sources.is_empty() {
        let _ = writeln!(
            o,
            "# 日付（YYYY-MM-DD・YYYY/MM/DD）の書かれた表・frontmatter・脚注・見出しは見つからなかった"
        );
        let _ = writeln!(o, "# [search.recent]");
        let _ = writeln!(
            o,
            "# date_sources = [\"table_row\", \"frontmatter:<キー>\", \"footnote_source\", \"heading\"]"
        );
    } else {
        for e in &evidence {
            let _ = writeln!(o, "# 根拠: {e}");
        }
        if !fm.is_empty() {
            let _ = writeln!(
                o,
                "# frontmatter のキーはページのすべての行に効く。更新日などのキーなら外す"
            );
        }
        let _ = writeln!(o, "[search.recent]");
        let _ = writeln!(o, "count        = 4");
        let _ = writeln!(
            o,
            "# pool       = 60   # 新しい記録は、上位の次のこの数の範囲から選ぶ（0 はすべて）"
        );
        let _ = writeln!(o, "date_sources = {}", toml_list(&sources));
    }
    let _ = writeln!(o);
    let _ = writeln!(
        o,
        "# 関連度によらず必ず出す範囲（主題のページの、見出しに語を含む節と、決まったページの上位）"
    );
    let _ = writeln!(o, "# [search.must_read]");
    let _ = writeln!(o, "# headings = [\"<見出しの語>\"]");
    let _ = writeln!(
        o,
        "# pages    = [{{ path = \"<glob>\", top = 3 }}]   # window = 0 を足すと当たった行だけ（1 行 1 件の表）"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_name_matching() {
        assert!(column_matches("呼び名", NAME_COLUMNS));
        assert!(column_matches("Names", NAME_COLUMNS));
        assert!(column_matches("別表記", NAME_COLUMNS));
        assert!(column_matches("使わなくなった表記", NAME_COLUMNS));
        assert!(column_matches("文字起こしの誤認識", MISREAD_COLUMNS));
        assert!(column_matches("誤", MISREAD_COLUMNS));
        assert!(column_matches("正", CONCEPT_COLUMNS));
        assert!(!column_matches("修正", CONCEPT_COLUMNS));
        assert!(!column_matches("filename", NAME_COLUMNS));
        assert!(!column_matches("説明", CONCEPT_COLUMNS));
    }

    #[test]
    fn toml_escaping() {
        assert_eq!(toml_str("a\"b\\c"), "\"a\\\"b\\\\c\"");
    }
}
