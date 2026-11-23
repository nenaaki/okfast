//! OKF バンドルの読み込み。

use crate::cache::{self, Stamp};
use crate::error::{Error, Result};
use crate::frontmatter::{self, Frontmatter};
use crate::sections::Outline;
use crate::textindex::TextIndex;
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// OKF 0.2 §3.1 の予約名（概念ではない）。
pub const RESERVED: [&str; 2] = ["index.md", "log.md"];

/// 1 つの概念ページ。frontmatter は初めて参照したときに解析する。
#[derive(Debug)]
pub struct Page {
    /// バンドル root からの相対パス（区切りは `/`）。
    pub rel: String,
    /// サイズと更新日時。
    pub stamp: Stamp,
    text: String,
    yaml: Option<(usize, usize)>,
    body_start: usize,
    body_line: usize,
    parsed: OnceLock<(Frontmatter, Option<String>)>,
    index: OnceLock<TextIndex>,
    outline: OnceLock<Outline>,
    /// 行の日付（日付の取り方の鍵, 各行の日付）。取り方が変わったら作り直す。
    dates: Mutex<Option<(u64, crate::dates::LineDates)>>,
}

impl Page {
    /// 検索用に正規化した本文（初めて参照したときに作る）。
    pub fn text_index(&self) -> &TextIndex {
        self.index.get_or_init(|| TextIndex::build(self.body()))
    }

    /// 見出しと節（初めて参照したときに作る）。
    pub fn outline(&self) -> &Outline {
        self.outline
            .get_or_init(|| Outline::build(self.body(), self.text_index()))
    }

    /// 行の日付を、取り方の鍵 `key` が同じなら使い回し、違えば `make` で作って覚える。
    pub fn cached_dates(
        &self,
        key: u64,
        make: impl FnOnce() -> Vec<Option<crate::dates::Date>>,
    ) -> crate::dates::LineDates {
        if let Ok(g) = self.dates.lock() {
            if let Some((k, d)) = g.as_ref() {
                if *k == key {
                    return Arc::clone(d);
                }
            }
        }
        let d: crate::dates::LineDates = make().into();
        if let Ok(mut g) = self.dates.lock() {
            *g = Some((key, Arc::clone(&d)));
        }
        d
    }

    /// 本文（frontmatter を除く）。
    pub fn body(&self) -> &str {
        &self.text[self.body_start..]
    }

    /// ファイルの全文。
    pub fn text(&self) -> &str {
        &self.text
    }

    /// 本文の最初の行の、ファイル上の行番号（1 始まり）。
    pub fn body_line(&self) -> usize {
        self.body_line
    }

    /// 本文の行を、ファイル上の行番号とともに返す。
    pub fn body_lines(&self) -> impl Iterator<Item = (usize, &str)> {
        let first = self.body_line;
        self.body()
            .lines()
            .enumerate()
            .map(move |(i, l)| (first + i, l))
    }

    fn parsed(&self) -> &(Frontmatter, Option<String>) {
        self.parsed.get_or_init(|| match self.yaml {
            None => (Frontmatter::default(), None),
            Some((s, e)) => match Frontmatter::parse(&self.text[s..e]) {
                Ok(fm) => (fm, None),
                Err(err) => (Frontmatter::default(), Some(err)),
            },
        })
    }

    /// frontmatter（なければ空）。
    pub fn frontmatter(&self) -> &Frontmatter {
        &self.parsed().0
    }

    /// frontmatter を読めなかったときの理由。
    pub fn frontmatter_error(&self) -> Option<&str> {
        self.parsed().1.as_deref()
    }

    /// 題名（frontmatter の `title`、なければファイル名）。
    pub fn title(&self) -> String {
        self.frontmatter().string("title").unwrap_or_else(|| {
            let name = self.rel.rsplit('/').next().unwrap_or(&self.rel);
            name.strip_suffix(".md").unwrap_or(name).to_string()
        })
    }

    fn new(rel: String, stamp: Stamp, text: String) -> Self {
        let (yaml, body_start, body_line) = match frontmatter::split(&text) {
            Some((y, body_start, body_line)) => {
                let s = y.as_ptr() as usize - text.as_ptr() as usize;
                (Some((s, s + y.len())), body_start, body_line)
            }
            None => (None, 0, 1),
        };
        Page {
            rel,
            stamp,
            text,
            yaml,
            body_start,
            body_line,
            parsed: OnceLock::new(),
            index: OnceLock::new(),
            outline: OnceLock::new(),
            dates: Mutex::new(None),
        }
    }
}

/// キャッシュの使い方。
#[derive(Debug, Clone, Default)]
pub enum CacheMode {
    /// 既定の場所（[`cache::default_dir`]）を使う。
    #[default]
    Default,
    /// 指定したディレクトリを使う。
    Dir(PathBuf),
    /// 使わない。
    Off,
}

/// 読み込みの内訳。
#[derive(Debug, Clone, Default)]
pub struct LoadStats {
    /// キャッシュから取ったページ数。
    pub cached: usize,
    /// ファイルから読んだページ数。
    pub read: usize,
    /// キャッシュにあったが消えたページ数。
    pub removed: usize,
    /// 使ったキャッシュファイル。
    pub cache_file: Option<PathBuf>,
    /// キャッシュを書き直したか。
    pub cache_written: bool,
    /// キャッシュを書けなかった理由（検索は続ける）。
    pub cache_error: Option<String>,
}

/// 読み込んだバンドル。
#[derive(Debug)]
pub struct Bundle {
    /// バンドルの root。
    pub root: PathBuf,
    /// 概念ページ（相対パス順）。
    pub pages: Vec<Arc<Page>>,
}

impl Bundle {
    /// キャッシュを使わずに読み込む。
    pub fn load(root: &Path) -> Result<Self> {
        Self::load_with(root, &CacheMode::Off).map(|(b, _)| b)
    }

    /// `root` 以下の `.md`（予約名と `.` で始まる名前を除く）を読み込む。
    pub fn load_with(root: &Path, mode: &CacheMode) -> Result<(Self, LoadStats)> {
        let mut files = Vec::new();
        walk(root, root, &mut files)?;
        files.sort_by(|a, b| a.0.cmp(&b.0));

        let cache_file = match mode {
            CacheMode::Off => None,
            CacheMode::Dir(d) => Some(cache::file_for(d, root)),
            CacheMode::Default => cache::default_dir().map(|d| cache::file_for(&d, root)),
        };
        let mut cached = cache_file.as_deref().and_then(cache::read);
        let had_cache = cached.is_some();
        let mut stats = LoadStats {
            cache_file: cache_file.clone(),
            ..LoadStats::default()
        };

        // キャッシュに同じサイズ・更新日時のものがあれば使い、なければ読む
        let slots: Vec<(String, PathBuf, Stamp, Option<String>)> = files
            .into_iter()
            .map(|(rel, path, stamp)| {
                let hit = cached
                    .as_mut()
                    .and_then(|m| m.remove(&rel))
                    .filter(|e| stamp.is_cacheable() && e.stamp == stamp)
                    .map(|e| e.text);
                (rel, path, stamp, hit)
            })
            .collect();
        stats.removed = cached.as_ref().map_or(0, |m| m.len());
        stats.cached = slots.iter().filter(|s| s.3.is_some()).count();
        stats.read = slots.len() - stats.cached;

        let pages = slots
            .into_par_iter()
            .map(|(rel, path, stamp, hit)| {
                let text = match hit {
                    Some(t) => t,
                    None => read_text(&path)?,
                };
                Ok(Arc::new(Page::new(rel, stamp, text)))
            })
            .collect::<Result<Vec<_>>>()?;

        if let Some(cf) = &cache_file {
            if stats.read > 0 || stats.removed > 0 || !had_cache {
                let entries = pages
                    .iter()
                    .filter(|p| p.stamp.is_cacheable())
                    .map(|p| (p.rel.as_str(), p.stamp, p.text.as_str()))
                    .collect::<Vec<_>>();
                match cache::write(cf, entries.into_iter()) {
                    Ok(()) => stats.cache_written = true,
                    Err(e) => stats.cache_error = Some(format!("{}: {e}", cf.display())),
                }
            }
        }

        Ok((
            Bundle {
                root: root.to_path_buf(),
                pages,
            },
            stats,
        ))
    }

    /// 相対パスでページを引く。
    pub fn page(&self, rel: &str) -> Option<&Page> {
        self.pages
            .binary_search_by(|p| p.rel.as_str().cmp(rel))
            .ok()
            .map(|i| &*self.pages[i])
    }

    /// 本文の合計バイト数。
    pub fn total_bytes(&self) -> usize {
        self.pages.iter().map(|p| p.text.len()).sum()
    }

    /// ディレクトリを走査し直す。サイズと更新日時が同じページは使い回し、ほかは読み直す。
    /// 返す値の 2 つ目は読み直したページの数。
    pub fn rescan(&self) -> Result<(Bundle, usize)> {
        let mut files = Vec::new();
        walk(&self.root, &self.root, &mut files)?;
        files.sort_by(|a, b| a.0.cmp(&b.0));
        let slots: Vec<_> = files
            .into_iter()
            .map(|(rel, path, stamp)| {
                let keep = self
                    .pages
                    .binary_search_by(|p| p.rel.as_str().cmp(&rel))
                    .ok()
                    .map(|i| &self.pages[i])
                    .filter(|p| stamp.is_cacheable() && p.stamp == stamp)
                    .cloned();
                (rel, path, stamp, keep)
            })
            .collect();
        let reread = slots.iter().filter(|s| s.3.is_none()).count();
        let pages = slots
            .into_par_iter()
            .map(|(rel, path, stamp, keep)| match keep {
                Some(p) => Ok(p),
                None => Ok(Arc::new(Page::new(rel, stamp, read_text(&path)?))),
            })
            .collect::<Result<Vec<_>>>()?;
        Ok((
            Bundle {
                root: self.root.clone(),
                pages,
            },
            reread,
        ))
    }

    /// 指定したページ（相対パス）だけを読み直す。消えていれば除き、新しければ加える。
    /// ページにならないパス（[`is_page_path`] が偽）は無視する。
    pub fn update(&self, rels: &[String]) -> Result<Bundle> {
        let mut pages = self.pages.clone();
        for rel in rels {
            if !is_page_path(rel) {
                continue;
            }
            let path = self.root.join(rel);
            let found = pages.binary_search_by(|p| p.rel.as_str().cmp(rel));
            match std::fs::metadata(&path) {
                Ok(m) if m.is_file() => {
                    let page = Arc::new(Page::new(
                        rel.clone(),
                        Stamp::from_metadata(&m),
                        read_text(&path)?,
                    ));
                    match found {
                        Ok(i) => pages[i] = page,
                        Err(i) => pages.insert(i, page),
                    }
                }
                _ => {
                    if let Ok(i) = found {
                        pages.remove(i);
                    }
                }
            }
        }
        Ok(Bundle {
            root: self.root.clone(),
            pages,
        })
    }
}

/// 相対パス（区切り `/`）がページになる名前か。`.` で始まる要素を含まず、`.md` で終わり、予約名でない。
pub fn is_page_path(rel: &str) -> bool {
    let mut parts = rel.split('/');
    let name = parts.next_back().unwrap_or("");
    rel.split('/').all(|c| !c.is_empty() && !c.starts_with('.'))
        && name.ends_with(".md")
        && !RESERVED.contains(&name)
}

fn read_text(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).map_err(|e| Error::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    Ok(match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    })
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf, Stamp)>) -> Result<()> {
    let io = |path: &Path| {
        let path = path.to_path_buf();
        move |e| Error::Io { path, source: e }
    };
    for entry in std::fs::read_dir(dir).map_err(io(dir))? {
        let entry = entry.map_err(io(dir))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        let ft = entry.file_type().map_err(io(&path))?;
        if ft.is_dir() {
            walk(root, &path, out)?;
        } else if name.ends_with(".md") && !RESERVED.contains(&name.as_ref()) {
            // Windows ではディレクトリの列挙で得た情報を返すので、ファイルを開かない
            let stamp = entry
                .metadata()
                .map(|m| Stamp::from_metadata(&m))
                .unwrap_or(Stamp {
                    size: 0,
                    mtime_ns: 0,
                });
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            out.push((rel, path, stamp));
        }
    }
    Ok(())
}
