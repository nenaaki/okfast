//! YAML frontmatter の切り出しと参照。
//!
//! OKF 0.2 §4.1: 必須は `type` だけで、未知のキーは拒否してはならない。
//! okfast は frontmatter を型を決めない YAML の値のまま保持し、任意のキーを参照できるようにする。

use yaml_rust2::{Yaml, YamlLoader};

/// 1 ページの frontmatter。
#[derive(Debug, Clone, Default)]
pub struct Frontmatter {
    root: Option<Yaml>,
}

impl Frontmatter {
    /// YAML の文字列から作る。
    pub fn parse(src: &str) -> Result<Self, String> {
        let docs = YamlLoader::load_from_str(src).map_err(|e| e.to_string())?;
        let root = docs.into_iter().next();
        match root {
            None | Some(Yaml::Hash(_)) | Some(Yaml::Null) => Ok(Self { root }),
            Some(_) => Err("frontmatter がマッピングでない".to_string()),
        }
    }

    /// キーの値（そのまま）。
    pub fn get(&self, key: &str) -> Option<&Yaml> {
        match &self.root {
            Some(Yaml::Hash(h)) => h.get(&Yaml::String(key.to_string())),
            _ => None,
        }
    }

    /// 入れ子のキーの値（`["generated", "at"]` なら `generated.at`）。
    pub fn get_path(&self, keys: &[&str]) -> Option<&Yaml> {
        let (first, rest) = keys.split_first()?;
        rest.iter().try_fold(self.get(first)?, |v, k| match v {
            Yaml::Hash(h) => h.get(&Yaml::String((*k).to_string())),
            _ => None,
        })
    }

    /// トップレベルのキー（文字列のもの）。
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        let hash = match &self.root {
            Some(Yaml::Hash(h)) => Some(h),
            _ => None,
        };
        hash.into_iter()
            .flat_map(|h| h.keys())
            .filter_map(Yaml::as_str)
    }

    /// キーの値が文字列の配列か（別名のキーの候補を探すのに使う）。
    pub fn is_string_list(&self, key: &str) -> bool {
        matches!(self.get(key), Some(Yaml::Array(a)) if !a.is_empty() && a.iter().all(|v| v.as_str().is_some()))
    }

    /// キーの値をスカラーの文字列の並びとして取る（スカラーなら 1 件、配列なら各要素）。
    pub fn strings(&self, key: &str) -> Vec<String> {
        match self.get(key) {
            Some(Yaml::Array(a)) => a.iter().filter_map(scalar).collect(),
            Some(v) => scalar(v).into_iter().collect(),
            None => Vec::new(),
        }
    }

    /// キーの値を 1 つの文字列として取る。
    pub fn string(&self, key: &str) -> Option<String> {
        self.get(key).and_then(scalar)
    }

    /// OKF の `type`。
    pub fn okf_type(&self) -> Option<String> {
        self.string("type")
    }

    /// frontmatter を持つか。
    pub fn is_present(&self) -> bool {
        self.root.is_some()
    }
}

fn scalar(v: &Yaml) -> Option<String> {
    match v {
        Yaml::String(s) | Yaml::Real(s) => Some(s.clone()),
        Yaml::Integer(i) => Some(i.to_string()),
        Yaml::Boolean(b) => Some(b.to_string()),
        _ => None,
    }
}

/// 文書を frontmatter（YAML 部分）と本文に分ける。
///
/// 戻り値: (YAML の文字列, 本文の開始バイト位置, 本文の開始行番号（1 始まり）)。
/// frontmatter がなければ `None`。
pub fn split(text: &str) -> Option<(&str, usize, usize)> {
    let text_start = if text.starts_with('\u{feff}') { 3 } else { 0 };
    let t = &text[text_start..];
    let first_end = t.find('\n')?;
    if t[..first_end].trim_end_matches('\r') != "---" {
        return None;
    }
    let yaml_start = first_end + 1;
    let mut pos = yaml_start;
    let mut line = 2;
    while pos <= t.len() {
        let end = t[pos..].find('\n').map_or(t.len(), |i| pos + i);
        if t[pos..end].trim_end_matches('\r') == "---" {
            let body = (end + 1).min(t.len());
            return Some((&t[yaml_start..pos], text_start + body, line + 1));
        }
        if end == t.len() {
            break;
        }
        pos = end + 1;
        line += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_and_parse() {
        let doc = "---\ntype: Term\ntags: [a, b]\ncredibility: B\ngenerated: { by: x, at: y }\n---\n# 見出し\n本文\n";
        let (y, body, line) = split(doc).unwrap();
        assert_eq!(&doc[body..], "# 見出し\n本文\n");
        assert_eq!(line, 7);
        let fm = Frontmatter::parse(y).unwrap();
        assert_eq!(fm.okf_type().as_deref(), Some("Term"));
        assert_eq!(fm.strings("tags"), vec!["a", "b"]);
        assert_eq!(fm.string("credibility").as_deref(), Some("B"));
        assert!(fm.get("generated").is_some());
    }

    #[test]
    fn crlf_and_missing() {
        let doc = "---\r\ntype: X\r\n---\r\nbody\r\n";
        let (y, body, line) = split(doc).unwrap();
        assert_eq!(&doc[body..], "body\r\n");
        assert_eq!(line, 4);
        assert!(Frontmatter::parse(y).unwrap().okf_type().is_some());
        assert!(split("# no frontmatter\n").is_none());
        assert!(split("---\ntype: X\n").is_none());
    }
}
