//! ページの選び方（パスの glob と任意の frontmatter キー）。
//!
//! 設定では `{ path = "glossary/**", type = "Term", category = ["用語集", "別名表"] }` のように書く。
//! `path` 以外のキーは frontmatter のキーとして扱い、値のどれかと一致すれば当たり（キー同士は AND）。
//! OKF の `type` は自由な値なので、okfast はどのキーも特別扱いしない。

use crate::error::{Error, Result};
use crate::frontmatter::Frontmatter;
use globset::{Glob, GlobBuilder, GlobSet, GlobSetBuilder};
use serde::Deserialize;
use std::collections::BTreeMap;

/// 1 つまたは複数の文字列。
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum OneOrMany {
    /// 1 つ。
    One(String),
    /// 複数。
    Many(Vec<String>),
}

impl OneOrMany {
    /// 文字列の並びにする。
    pub fn to_vec(&self) -> Vec<String> {
        match self {
            OneOrMany::One(s) => vec![s.clone()],
            OneOrMany::Many(v) => v.clone(),
        }
    }
}

/// 設定に書かれたページの選び方。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(transparent)]
pub struct SelectorSpec(pub BTreeMap<String, OneOrMany>);

/// コンパイル済みのページの選び方。
#[derive(Debug, Clone)]
pub struct Selector {
    path: Option<GlobSet>,
    keys: Vec<(String, Vec<String>)>,
    description: String,
}

/// glob をコンパイルする（`*` は `/` をまたがない。`**` はまたぐ）。
pub fn compile_glob(pattern: &str) -> Result<Glob> {
    GlobBuilder::new(pattern)
        .literal_separator(true)
        .build()
        .map_err(|e| Error::Glob {
            pattern: pattern.to_string(),
            message: e.to_string(),
        })
}

/// 複数の glob をまとめてコンパイルする。
pub fn compile_globset(patterns: &[String]) -> Result<GlobSet> {
    let mut b = GlobSetBuilder::new();
    for p in patterns {
        b.add(compile_glob(p)?);
    }
    b.build().map_err(|e| Error::Glob {
        pattern: patterns.join(", "),
        message: e.to_string(),
    })
}

impl Selector {
    /// 設定からコンパイルする。空なら全ページに当たる。
    pub fn compile(spec: &SelectorSpec) -> Result<Self> {
        let mut path = None;
        let mut keys = Vec::new();
        for (k, v) in &spec.0 {
            if k == "path" {
                path = Some(compile_globset(&v.to_vec())?);
            } else {
                keys.push((k.clone(), v.to_vec()));
            }
        }
        let description = if spec.0.is_empty() {
            "すべてのページ".to_string()
        } else {
            spec.0
                .iter()
                .map(|(k, v)| format!("{k}={}", v.to_vec().join("|")))
                .collect::<Vec<_>>()
                .join(" かつ ")
        };
        Ok(Selector {
            path,
            keys,
            description,
        })
    }

    /// ページが当たるか。
    pub fn matches(&self, rel: &str, fm: &Frontmatter) -> bool {
        if let Some(g) = &self.path {
            if !g.is_match(rel) {
                return false;
            }
        }
        self.keys.iter().all(|(k, want)| {
            let have = fm.strings(k);
            have.iter().any(|h| want.iter().any(|w| w == h))
        })
    }

    /// 人が読む説明。
    pub fn description(&self) -> &str {
        &self.description
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(src: &str) -> SelectorSpec {
        #[derive(Deserialize)]
        struct W {
            s: SelectorSpec,
        }
        toml::from_str::<W>(src).unwrap().s
    }

    #[test]
    fn path_and_keys() {
        let s = Selector::compile(&spec(
            r#"s = { path = "glossary/*.md", type = ["Term", "Glossary"] }"#,
        ))
        .unwrap();
        let term = Frontmatter::parse("type: Term").unwrap();
        let other = Frontmatter::parse("type: Requirement").unwrap();
        assert!(s.matches("glossary/a.md", &term));
        assert!(!s.matches("glossary/x/a.md", &term));
        assert!(!s.matches("glossary/a.md", &other));
        let all = Selector::compile(&SelectorSpec::default()).unwrap();
        assert!(all.matches("x/y.md", &Frontmatter::default()));
    }
}
