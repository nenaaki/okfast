//! 本文から必要な構造だけを取り出す軽量な Markdown 解析。

/// GFM の表。
#[derive(Debug, Clone)]
pub struct Table {
    /// 見出し行のセル。
    pub header: Vec<String>,
    /// 見出し行の行番号（1 始まり）。
    pub header_line: usize,
    /// データ行。
    pub rows: Vec<Row>,
}

/// 表の 1 行。
#[derive(Debug, Clone)]
pub struct Row {
    /// ファイル上の行番号（1 始まり）。
    pub line: usize,
    /// セル（前後の空白は除去済み）。
    pub cells: Vec<String>,
}

impl Table {
    /// 見出しの名前から列番号を引く。
    pub fn column(&self, name: &str) -> Option<usize> {
        self.header.iter().position(|h| h == name)
    }
}

/// 行の並びから表を取り出す（フェンスで囲まれたコードの中は除く）。
pub fn tables<'a>(lines: impl Iterator<Item = (usize, &'a str)>) -> Vec<Table> {
    let mut out = Vec::new();
    let mut in_fence = false;
    let mut current: Option<Table> = None;
    let mut pending_header: Option<(usize, Vec<String>)> = None;

    for (no, raw) in lines {
        let line = raw.trim();
        if line.starts_with("```") || line.starts_with("~~~") {
            in_fence = !in_fence;
            flush(&mut current, &mut out);
            pending_header = None;
            continue;
        }
        if in_fence || !line.starts_with('|') {
            flush(&mut current, &mut out);
            pending_header = None;
            continue;
        }
        let cells = split_row(line);
        if let Some(t) = current.as_mut() {
            t.rows.push(Row { line: no, cells });
        } else if let Some((header_line, header)) = pending_header.take() {
            if is_separator(&cells) {
                current = Some(Table {
                    header,
                    header_line,
                    rows: Vec::new(),
                });
            } else {
                pending_header = Some((no, cells));
            }
        } else {
            pending_header = Some((no, cells));
        }
    }
    flush(&mut current, &mut out);
    out
}

fn flush(current: &mut Option<Table>, out: &mut Vec<Table>) {
    if let Some(t) = current.take() {
        out.push(t);
    }
}

fn is_separator(cells: &[String]) -> bool {
    !cells.is_empty()
        && cells.iter().all(|c| {
            let c = c.trim();
            c.contains('-') && c.chars().all(|ch| matches!(ch, '-' | ':' | ' '))
        })
}

/// `| a | b \| c |` → ["a", "b | c"]
fn split_row(line: &str) -> Vec<String> {
    let inner = line.strip_prefix('|').unwrap_or(line);
    let inner = inner.strip_suffix('|').unwrap_or(inner);
    let mut cells = Vec::new();
    let mut cur = String::new();
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => {
                cur.push('|');
                chars.next();
            }
            '|' => cells.push(std::mem::take(&mut cur).trim().to_string()),
            _ => cur.push(c),
        }
    }
    cells.push(cur.trim().to_string());
    cells
}

/// 脚注の定義 `[^id]: 本文` なら（id, 本文）。
pub fn footnote_def(line: &str) -> Option<(&str, &str)> {
    let t = line.trim_start();
    let rest = t.strip_prefix("[^")?;
    let close = rest.find("]:")?;
    Some((&rest[..close], &rest[close + 2..]))
}

/// リンクの行き先 `](...)` の中の文字列（[`strip_link_targets`] が取り除くもの）。
pub fn link_target_texts(s: &str) -> impl Iterator<Item = &str> {
    let mut rest = s;
    std::iter::from_fn(move || {
        loop {
            let i = rest.find("](")?;
            let after = &rest[i + 2..];
            let mut depth = 1usize;
            let close = after.char_indices().find_map(|(k, c)| {
                match c {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(k);
                        }
                    }
                    _ => {}
                }
                None
            });
            match close {
                Some(k) => {
                    rest = &after[k + 1..];
                    return Some(&after[..k]);
                }
                None => rest = after,
            }
        }
    })
}

/// リンクの行き先 `](...)` を取り除き、`]` だけを残す（`[表示](行き先)` → `[表示]`）。行き先の中の括弧の
/// 組は数える。閉じていない `](` はそのまま残す。リンクがなければ借りたまま返す。
pub fn strip_link_targets(s: &str) -> std::borrow::Cow<'_, str> {
    if !s.contains("](") {
        return std::borrow::Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("](") {
        let after = &rest[i + 2..];
        let mut depth = 1usize;
        let close = after.char_indices().find_map(|(k, c)| {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(k);
                    }
                }
                _ => {}
            }
            None
        });
        match close {
            Some(k) => {
                out.push_str(&rest[..=i]);
                rest = &after[k + 1..];
            }
            None => {
                out.push_str(&rest[..i + 2]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    std::borrow::Cow::Owned(out)
}

/// 脚注参照 `[^id]` を取り除く。
pub fn strip_footnotes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("[^") {
        out.push_str(&rest[..i]);
        match rest[i..].find(']') {
            Some(j) => rest = &rest[i + j + 1..],
            None => {
                rest = &rest[i..];
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// セルの文字列から、語として使う文字だけを取り出す（脚注参照を除き、リンクは表示の文字にする）。
pub fn cell_text(s: &str) -> String {
    let s = strip_footnotes(s);
    let mut out = String::with_capacity(s.len());
    let mut rest = s.as_str();
    while let Some(open) = rest.find('[') {
        let after_open = &rest[open + 1..];
        let link = after_open.find("](").and_then(|close| {
            let after = &after_open[close + 2..];
            after.find(')').map(|end| (close, close + 2 + end + 1))
        });
        match link {
            // `[` と `](` の間に別の `[` がなければリンクとみなす
            Some((close, consumed)) if !after_open[..close].contains('[') => {
                out.push_str(&rest[..open]);
                out.push_str(&after_open[..close]);
                rest = &after_open[consumed..];
            }
            _ => {
                out.push_str(&rest[..=open]);
                rest = after_open;
            }
        }
    }
    out.push_str(rest);
    out
}

/// 「該当なし」を表すダッシュだけのセルか（—・–・―・-・‐・−）。
pub fn is_placeholder(s: &str) -> bool {
    let t = s.trim();
    !t.is_empty()
        && t.chars()
            .all(|c| matches!(c, '—' | '–' | '―' | '-' | '‐' | '−'))
}

/// 語から注記（開き括弧から対応する閉じ括弧まで）を外す。
pub fn strip_notes(s: &str, openers: &[String]) -> String {
    if openers.is_empty() {
        return s.to_string();
    }
    let closer = |o: &str| match o {
        "（" => "）",
        "(" => ")",
        "【" => "】",
        "[" => "]",
        "「" => "」",
        "〔" => "〕",
        _ => "",
    };
    let mut out = s.to_string();
    for o in openers {
        let c = closer(o);
        if c.is_empty() {
            continue;
        }
        while let Some(i) = out.find(o.as_str()) {
            match out[i + o.len()..].find(c) {
                Some(j) => out.replace_range(i..i + o.len() + j + c.len(), ""),
                None => break,
            }
        }
    }
    out.trim().to_string()
}

/// セル中のリンク `[text](target)` の行き先を、バンドル root からの相対パスで返す。
///
/// `/` で始まる行き先はバンドル root から、それ以外は `page`（リンクを含むページ）のディレクトリから解決する
/// （OKF 0.2 §6.1）。`#以降` は除き、`%XX` は戻す。外部 URL は除く。
pub fn link_targets(s: &str, page: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(i) = rest.find("](") {
        let after = &rest[i + 2..];
        let Some(j) = after.find(')') else { break };
        let target = after[..j].trim();
        let target = target.split('#').next().unwrap_or("");
        if !target.is_empty() && !target.contains("://") && !target.starts_with("mailto:") {
            if let Some(t) = resolve(page, &percent_decode(target)) {
                out.push(t);
            }
        }
        rest = &after[j + 1..];
    }
    out
}

fn resolve(page: &str, target: &str) -> Option<String> {
    let mut parts: Vec<&str> = if target.starts_with('/') {
        Vec::new()
    } else {
        let mut v: Vec<&str> = page.split('/').collect();
        v.pop();
        v
    };
    for seg in target.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

fn percent_decode(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok();
            if let Some(v) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn footnotes_and_link_targets() {
        assert_eq!(
            footnote_def("[^m1]: meetings/2026-01-15.md"),
            Some(("m1", " meetings/2026-01-15.md"))
        );
        assert_eq!(footnote_def("本文[^m1]"), None);
        assert_eq!(
            strip_link_targets("[価格](/pricing/price.md) と [x](a(b)c) の後"),
            "[価格] と [x] の後"
        );
        assert_eq!(strip_link_targets("閉じない](abc"), "閉じない](abc");
        let t: Vec<&str> =
            link_target_texts("[価格](/pricing/price.md) と [x](a(b)c) の後 ](閉じない").collect();
        assert_eq!(t, vec!["/pricing/price.md", "a(b)c"]);
        assert!(matches!(
            strip_link_targets("リンクなし"),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    #[test]
    fn parse_tables() {
        let src = "前文\n| 概念 | 呼び名 |\n|---|:--:|\n| A | a、b[^g] |\n| B \\| x | c |\n\n```\n| x | y |\n|---|---|\n```\n";
        let ts = tables(src.lines().enumerate().map(|(i, l)| (i + 1, l)));
        assert_eq!(ts.len(), 1);
        assert_eq!(ts[0].header, vec!["概念", "呼び名"]);
        assert_eq!(ts[0].header_line, 2);
        assert_eq!(ts[0].rows.len(), 2);
        assert_eq!(ts[0].rows[0].line, 4);
        assert_eq!(ts[0].rows[1].cells[0], "B | x");
        assert_eq!(strip_footnotes(&ts[0].rows[0].cells[1]), "a、b");
    }

    #[test]
    fn links() {
        assert_eq!(
            link_targets("[x](/a/b.md)、[y](c.md#h) [z](https://e)", "g/p.md"),
            vec!["a/b.md", "g/c.md"]
        );
        assert_eq!(
            link_targets("[x](../n/w.md)", "g/sub/p.md"),
            vec!["g/n/w.md"]
        );
        assert_eq!(
            link_targets("[x](./%E7%94%A8.md)", "g/p.md"),
            vec!["g/用.md"]
        );
        assert!(link_targets("[x](../../out.md)", "p.md").is_empty());
    }

    #[test]
    fn cell_text_and_placeholders() {
        assert_eq!(cell_text("[Sprocket](a.md)、b[^x]"), "Sprocket、b");
        assert_eq!(cell_text("[x] と [y](z.md)"), "[x] と y");
        assert!(is_placeholder(" — "));
        assert!(is_placeholder("-"));
        assert!(!is_placeholder("ー"));
        assert!(!is_placeholder("A-B"));
    }

    #[test]
    fn notes() {
        let o = vec!["（".to_string(), "(".to_string()];
        assert_eq!(strip_notes("Widget Pro（旧称）", &o), "Widget Pro");
        assert_eq!(strip_notes("WP(略称)", &o), "WP");
        assert_eq!(strip_notes("定例会（本社）", &[]), "定例会（本社）");
        assert_eq!(strip_notes("閉じない（", &o), "閉じない（");
    }
}
