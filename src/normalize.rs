//! 照合のための文字列の正規化。
//!
//! - 通常の語: NFKC → 小文字化 → 空白の除去（「検索 API」と「検索API」、全角半角を同一視）
//! - 短い英数字の略語（WP・AB など）: NFKC のみ。大文字小文字を区別し、前後が英数字でないときだけ当てる

use std::borrow::Cow;
use unicode_normalization::UnicodeNormalization;

/// NFKC 正規化。ASCII だけの文字列は複製しない。
pub fn nfkc(s: &str) -> Cow<'_, str> {
    if s.is_ascii() {
        Cow::Borrowed(s)
    } else {
        Cow::Owned(s.nfkc().collect())
    }
}

/// NFKC 済みの文字列を小文字化し、空白を除く。
pub fn fold_nfkc(s: &str, out: &mut String) {
    out.clear();
    fold_append(s, out);
}

/// NFKC 済みの文字列を小文字化し、空白を除いて `out` に足す。
pub fn fold_append(s: &str, out: &mut String) {
    if s.is_ascii() {
        out.extend(
            s.bytes()
                .filter(|b| !b.is_ascii_whitespace())
                .map(|b| b.to_ascii_lowercase() as char),
        );
        return;
    }
    for c in s.chars() {
        if c.is_ascii() {
            if !c.is_ascii_whitespace() {
                out.push(c.to_ascii_lowercase());
            }
        } else if is_caseless_cjk(c) {
            out.push(c);
        } else if !c.is_whitespace() {
            out.extend(c.to_lowercase());
        }
    }
}

/// 大文字小文字がなく空白でもない文字の範囲（かな・CJK 記号・漢字）。前処理を省くため。
#[inline]
fn is_caseless_cjk(c: char) -> bool {
    ('\u{3001}'..='\u{9FFF}').contains(&c)
}

/// NFKC → 小文字化 → 空白の除去。
pub fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    fold_nfkc(&nfkc(s), &mut out);
    out
}

/// 短い略語として扱うか（ASCII 英数字と `.` `-` だけで、`max` 文字以下）。
pub fn is_short(s: &str, max: usize) -> bool {
    let n = nfkc(s);
    let t = n.trim();
    !t.is_empty()
        && t.len() <= max
        && t.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
}

/// `hay`（NFKC 済み）の中に、前後が ASCII 英数字でない `needle` があるか。
pub fn contains_token(hay: &str, needle: &str) -> bool {
    let mut found = false;
    for_each_token(hay, needle, |_| {
        found = true;
        false
    });
    found
}

/// `hay` の中の、前後が ASCII 英数字でない `needle` の位置ごとに `f` を呼ぶ。
/// `f` が `false` を返したら止める。
pub fn for_each_token(hay: &str, needle: &str, mut f: impl FnMut(usize) -> bool) {
    if needle.is_empty() {
        return;
    }
    let h = hay.as_bytes();
    let mut from = 0;
    while let Some(pos) = hay[from..].find(needle) {
        let start = from + pos;
        let end = start + needle.len();
        let before_ok = start == 0 || !h[start - 1].is_ascii_alphanumeric();
        let after_ok = end == h.len() || !h[end].is_ascii_alphanumeric();
        if before_ok && after_ok && !f(start) {
            return;
        }
        from = start + 1;
        while !hay.is_char_boundary(from) {
            from += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fold_ignores_width_case_space() {
        assert_eq!(fold("検索 ＡＰＩ"), fold("検索api"));
        assert_eq!(fold("Widget Pro"), "widgetpro");
        assert_eq!(fold("ｗｉｄｇｅｔ　X"), "widgetx");
    }

    #[test]
    fn short_tokens() {
        assert!(is_short("WP", 3));
        assert!(is_short("ＡＢ", 3));
        assert!(!is_short("WPXX", 3));
        assert!(!is_short("営業", 3));
    }

    #[test]
    fn caseless_cjk_range_is_really_caseless() {
        for c in ('\u{3001}'..='\u{9FFF}').filter(|c| is_caseless_cjk(*c)) {
            assert!(!c.is_whitespace(), "{c:?}");
            assert!(c.to_lowercase().eq([c]), "{c:?}");
        }
    }

    #[test]
    fn token_boundaries() {
        assert!(contains_token("AB と WP の話", "WP"));
        assert!(contains_token("WP", "WP"));
        assert!(!contains_token("WPA の話", "WP"));
        assert!(!contains_token("XWP", "WP"));
        assert!(contains_token("（WP）", "WP"));
        assert!(!contains_token("wp", "WP"));
    }
}
