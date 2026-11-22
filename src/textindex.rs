//! 検索用に正規化した本文。
//!
//! ページごとに 1 回だけ作り、[`crate::bundle::Page::text_index`] で使い回す。
//! 検索は行ごとに文字列を作らず、正規化済みの連続した文字列を一度なめるだけになる。
//!
//! - `folded`: 各行を NFKC → 小文字化 → 空白除去し、`\n` でつないだもの（通常の語の照合用）
//! - `nfkc`: 各行を NFKC だけにし、`\n` でつないだもの（短い略語の照合用）
//!
//! 照合する語は空白（`\n` を含む）を持たないので、行をまたいで当たることはない。
//! 行ごとに正規化してつなぐので、結果は行ごとに正規化した場合と一致する。

use crate::normalize::fold_append;
use unicode_normalization::{IsNormalized, UnicodeNormalization, is_nfkc_quick};

/// 1 ページの正規化済みの本文。
#[derive(Debug, Default)]
pub struct TextIndex {
    /// 本文での各行の開始位置（バイト）。
    orig: Vec<u32>,
    /// 各行の終わり（`\r` を除いた位置）。
    orig_end: Vec<u32>,
    folded: String,
    folded_starts: Vec<u32>,
    /// NFKC の結果が本文と同じなら `None`（本文をそのまま使う）。
    nfkc: Option<String>,
    nfkc_starts: Vec<u32>,
}

impl TextIndex {
    /// 本文から作る。行の区切りは [`str::lines`] と同じ（`\n`、直前の `\r` は除く）。
    pub fn build(body: &str) -> Self {
        let mut orig = Vec::new();
        let mut orig_end = Vec::new();
        let mut off = 0usize;
        for raw in body.split_inclusive('\n') {
            let line = raw.strip_suffix('\n').unwrap_or(raw);
            let line = line.strip_suffix('\r').unwrap_or(line);
            orig.push(off as u32);
            orig_end.push((off + line.len()) as u32);
            off += raw.len();
        }

        let normalized = matches!(is_nfkc_quick(body.chars()), IsNormalized::Yes);
        let mut nfkc = (!normalized).then(|| String::with_capacity(body.len()));
        let mut nfkc_starts = Vec::new();
        let mut folded = String::with_capacity(body.len());
        let mut folded_starts = Vec::with_capacity(orig.len());
        let mut tmp = String::new();
        for (&s, &e) in orig.iter().zip(&orig_end) {
            let line = &body[s as usize..e as usize];
            let n: &str = match &mut nfkc {
                None => line,
                Some(out) => {
                    nfkc_starts.push(out.len() as u32);
                    let start = out.len();
                    if line.is_ascii() || matches!(is_nfkc_quick(line.chars()), IsNormalized::Yes) {
                        out.push_str(line);
                    } else {
                        tmp.clear();
                        tmp.extend(line.nfkc());
                        out.push_str(&tmp);
                    }
                    out.push('\n');
                    &out[start..out.len() - 1]
                }
            };
            folded_starts.push(folded.len() as u32);
            fold_append(n, &mut folded);
            folded.push('\n');
        }
        if nfkc.is_none() {
            nfkc_starts = orig.clone();
        }
        TextIndex {
            orig,
            orig_end,
            folded,
            folded_starts,
            nfkc,
            nfkc_starts,
        }
    }

    /// 行数。
    pub fn lines(&self) -> usize {
        self.orig.len()
    }

    /// 正規化（NFKC・小文字・空白除去）した本文。行は `\n` で区切られる。
    pub fn folded(&self) -> &str {
        &self.folded
    }

    /// NFKC だけの本文。`body` には元の本文を渡す。
    pub fn nfkc<'a>(&'a self, body: &'a str) -> &'a str {
        self.nfkc.as_deref().unwrap_or(body)
    }

    /// `folded` の位置が何行目か（0 始まり）。
    pub fn folded_line(&self, pos: usize) -> usize {
        line_of(&self.folded_starts, pos)
    }

    /// `folded` での i 行目（0 始まり）の範囲（行末の改行を含まない）。
    pub fn folded_line_range(&self, i: usize) -> (usize, usize) {
        let start = self.folded_starts[i] as usize;
        let end = self
            .folded_starts
            .get(i + 1)
            .map_or(self.folded.len(), |&s| s as usize)
            - 1;
        (start, end)
    }

    /// `nfkc` の位置が何行目か（0 始まり）。
    pub fn nfkc_line(&self, pos: usize) -> usize {
        line_of(&self.nfkc_starts, pos)
    }

    /// i 行目（0 始まり）の元の内容。
    pub fn line<'a>(&self, body: &'a str, i: usize) -> &'a str {
        &body[self.orig[i] as usize..self.orig_end[i] as usize]
    }

    /// 保持しているバイト数（目安）。
    pub fn heap_bytes(&self) -> usize {
        self.folded.capacity()
            + self.nfkc.as_ref().map_or(0, String::capacity)
            + 4 * (self.orig.capacity()
                + self.orig_end.capacity()
                + self.folded_starts.capacity()
                + self.nfkc_starts.capacity())
    }
}

fn line_of(starts: &[u32], pos: usize) -> usize {
    starts
        .partition_point(|&s| s as usize <= pos)
        .saturating_sub(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalize::{fold, nfkc};

    fn check(body: &str) {
        let ix = TextIndex::build(body);
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(ix.lines(), lines.len(), "{body:?}");
        let folded: Vec<&str> = ix.folded().split_terminator('\n').collect();
        let n: Vec<&str> = ix.nfkc(body).split_terminator('\n').collect();
        for (i, l) in lines.iter().enumerate() {
            assert_eq!(ix.line(body, i), *l);
            assert_eq!(folded[i], fold(l));
            assert_eq!(n[i].trim_end_matches('\r'), nfkc(l));
            assert_eq!(ix.folded_line(ix.folded_starts[i] as usize), i);
            let (a, b) = ix.folded_line_range(i);
            assert_eq!(&ix.folded()[a..b], folded[i]);
            assert_eq!(ix.nfkc_line(ix.nfkc_starts[i] as usize), i);
        }
    }

    #[test]
    fn matches_per_line_normalization() {
        check("");
        check("a\n");
        check("Widget Pro\nｗｉｄｇｅｔ　ＰＲＯ\n\n検索 API\r\nlast");
        check("全角ＡＢＣ\r\nｶﾀｶﾅ\n㍿\n");
        check("plain ascii\nonly\n");
        check("\n\n\n");
    }
}
