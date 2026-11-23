//! 本文を見出しで節に分ける。
//!
//! - 見出しは ATX 形式（行頭の空白 3 つまで、`#` 1〜6 個、続けて空白か行末）。フェンス（```` ``` ```` か `~~~`）の中は除く
//! - 節は、見出しの行から次の見出し（レベルを問わない）の手前まで。いちばん細かい単位で区切る
//! - 最初の見出しより前の本文は、見出しなしの節にする（空行だけなら節にしない）
//! - 節の終わりは、節の最後の空でない行
//!
//! 行番号は本文の中の 0 始まり（ファイル上の行番号は [`crate::bundle::Page::body_line`] を足す）。

use crate::textindex::TextIndex;

/// 見出し。
#[derive(Debug, Clone)]
pub struct Heading {
    /// 本文の中の行（0 始まり）。
    pub line: usize,
    /// レベル（`#` の数）。
    pub level: u8,
    /// 見出しの文字（`#` と前後の空白、閉じの `#` を除く）。
    pub text: String,
    /// 親の見出し（[`Outline::headings`] の添え字）。
    pub parent: Option<usize>,
}

/// 節。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Section {
    /// 最初の行（0 始まり。見出しがあれば見出しの行）。
    pub start: usize,
    /// 最後の空でない行（0 始まり、この行を含む）。
    pub end: usize,
    /// 見出し（[`Outline::headings`] の添え字）。最初の見出しより前の節は `None`。
    pub heading: Option<usize>,
}

impl Section {
    /// 行数。
    pub fn len(&self) -> usize {
        self.end + 1 - self.start
    }

    /// 空か（常に偽。節は少なくとも 1 行ある）。
    pub fn is_empty(&self) -> bool {
        false
    }
}

/// 1 ページの見出しと節。
#[derive(Debug, Clone, Default)]
pub struct Outline {
    /// 見出し（行の順）。
    pub headings: Vec<Heading>,
    /// 節（行の順）。
    pub sections: Vec<Section>,
}

impl Outline {
    /// 本文から作る。
    pub fn build(body: &str, ix: &TextIndex) -> Self {
        let n = ix.lines();
        let mut headings: Vec<Heading> = Vec::new();
        let mut stack: Vec<usize> = Vec::new();
        let mut fence: Option<(u8, usize)> = None;
        for i in 0..n {
            let line = ix.line(body, i);
            if let Some(f) = fence_marker(line) {
                match fence {
                    None => fence = Some(f),
                    Some((c, len)) if c == f.0 && f.1 >= len => fence = None,
                    Some(_) => {}
                }
                continue;
            }
            if fence.is_some() {
                continue;
            }
            if let Some((level, text)) = heading_of(line) {
                while stack.last().is_some_and(|&h| headings[h].level >= level) {
                    stack.pop();
                }
                headings.push(Heading {
                    line: i,
                    level,
                    text: text.to_string(),
                    parent: stack.last().copied(),
                });
                stack.push(headings.len() - 1);
            }
        }

        let last_nonblank = |from: usize, to: usize| -> Option<usize> {
            (from..to)
                .rev()
                .find(|&i| !ix.line(body, i).trim().is_empty())
        };
        let mut sections = Vec::with_capacity(headings.len() + 1);
        let first = headings.first().map_or(n, |h| h.line);
        if let Some(end) = last_nonblank(0, first) {
            let start = (0..first)
                .find(|&i| !ix.line(body, i).trim().is_empty())
                .unwrap_or(0);
            sections.push(Section {
                start,
                end,
                heading: None,
            });
        }
        for (k, h) in headings.iter().enumerate() {
            let next = headings.get(k + 1).map_or(n, |x| x.line);
            sections.push(Section {
                start: h.line,
                end: last_nonblank(h.line, next).unwrap_or(h.line),
                heading: Some(k),
            });
        }
        Outline { headings, sections }
    }

    /// 行（0 始まり）が属する節の添え字。最初の節より前の空行は最初の節にする。
    pub fn section_of(&self, line: usize) -> Option<usize> {
        if self.sections.is_empty() {
            return None;
        }
        Some(
            self.sections
                .partition_point(|s| s.start <= line)
                .saturating_sub(1),
        )
    }

    /// 見出しとその祖先（近い順）。
    pub fn ancestors(&self, heading: usize) -> impl Iterator<Item = &Heading> {
        let mut cur = Some(heading);
        std::iter::from_fn(move || {
            let h = &self.headings[cur?];
            cur = h.parent;
            Some(h)
        })
    }
}

/// ATX 見出しなら（レベル, 文字）。
pub fn heading_of(line: &str) -> Option<(u8, &str)> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let s = &line[indent..];
    let level = s.bytes().take_while(|&b| b == b'#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let rest = &s[level..];
    if !(rest.is_empty() || rest.starts_with([' ', '\t'])) {
        return None;
    }
    let mut text = rest.trim();
    // 閉じの # 列（前が空白のときだけ）
    let without = text.trim_end_matches('#');
    if without.len() < text.len() && (without.is_empty() || without.ends_with([' ', '\t'])) {
        text = without.trim_end();
    }
    Some((level as u8, text))
}

/// フェンスの開き・閉じの行なら（文字, 長さ）。
fn fence_marker(line: &str) -> Option<(u8, usize)> {
    let t = line.trim_start_matches(' ');
    if line.len() - t.len() > 3 {
        return None;
    }
    let c = *t.as_bytes().first()?;
    if c != b'`' && c != b'~' {
        return None;
    }
    let len = t.bytes().take_while(|&b| b == c).count();
    (len >= 3).then_some((c, len))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outline(body: &str) -> Outline {
        Outline::build(body, &TextIndex::build(body))
    }

    #[test]
    fn headings() {
        assert_eq!(heading_of("# a"), Some((1, "a")));
        assert_eq!(heading_of("### 未決 ###"), Some((3, "未決")));
        assert_eq!(heading_of("   ## x"), Some((2, "x")));
        assert_eq!(heading_of("##"), Some((2, "")));
        assert_eq!(heading_of("## C#"), Some((2, "C#")));
        assert_eq!(heading_of("#tag"), None);
        assert_eq!(heading_of("    # code"), None);
        assert_eq!(heading_of("####### seven"), None);
    }

    #[test]
    fn sections_and_parents() {
        let body = "前文\n\n# A\na1\n\n## B\nb1\n```\n# not heading\n```\n\n# C\n";
        let o = outline(body);
        let names: Vec<&str> = o.headings.iter().map(|h| h.text.as_str()).collect();
        assert_eq!(names, ["A", "B", "C"]);
        assert_eq!(o.headings[1].parent, Some(0));
        assert_eq!(o.headings[2].parent, None);
        assert_eq!(
            o.sections,
            vec![
                Section {
                    start: 0,
                    end: 0,
                    heading: None
                },
                Section {
                    start: 2,
                    end: 3,
                    heading: Some(0)
                },
                Section {
                    start: 5,
                    end: 9,
                    heading: Some(1)
                },
                Section {
                    start: 11,
                    end: 11,
                    heading: Some(2)
                },
            ]
        );
        assert_eq!(o.section_of(4), Some(1));
        assert_eq!(o.section_of(8), Some(2));
        let up: Vec<&str> = o.ancestors(1).map(|h| h.text.as_str()).collect();
        assert_eq!(up, ["B", "A"]);
    }

    #[test]
    fn no_preamble_and_empty() {
        let o = outline("\n\n# A\nx\n");
        assert_eq!(o.sections.len(), 1);
        assert_eq!(o.section_of(0), Some(0));
        assert!(outline("").sections.is_empty());
        assert_eq!(outline("本文だけ\n").sections.len(), 1);
    }
}
