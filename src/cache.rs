//! 読み込み済みのページのキャッシュ。
//!
//! Windows ではファイルを 1 つ開くたびのコストが大きい（数百ファイルで数百 ms）。
//! 全ページの本文を 1 つのキャッシュファイルにまとめ、変わったファイルだけを読み直す。
//!
//! - 変わったかどうかは、ディレクトリの走査で得たサイズと更新日時で判断する
//!   （Windows ではディレクトリの列挙で取れるので、ファイルを開かない）
//! - キャッシュはバンドルの中に置かない。既定は `%LOCALAPPDATA%\okfast`（`OKFAST_CACHE_DIR` で変更可）
//! - キャッシュが壊れていたり形式が古かったりしたら、黙って作り直す

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 8] = b"OKFASTC\0";
const VERSION: u32 = 1;

/// ファイルのサイズと更新日時。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    /// バイト数。
    pub size: u64,
    /// UNIX 時刻からのナノ秒。取れないときは 0（キャッシュしない）。
    pub mtime_ns: u64,
}

impl Stamp {
    /// メタデータから作る。
    pub fn from_metadata(m: &std::fs::Metadata) -> Self {
        let mtime_ns = m
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos() as u64);
        Stamp {
            size: m.len(),
            mtime_ns,
        }
    }

    /// キャッシュに使えるか。
    pub fn is_cacheable(&self) -> bool {
        self.mtime_ns != 0
    }
}

/// キャッシュの置き場所の既定値。
pub fn default_dir() -> Option<PathBuf> {
    let env = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    if let Some(d) = env("OKFAST_CACHE_DIR") {
        return Some(d);
    }
    env("LOCALAPPDATA")
        .or_else(|| env("XDG_CACHE_HOME"))
        .or_else(|| env("HOME").map(|h| h.join(".cache")))
        .map(|d| d.join("okfast"))
}

/// バンドルの root に対応するキャッシュファイルのパス。
pub fn file_for(dir: &Path, root: &Path) -> PathBuf {
    let key = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let key = key.to_string_lossy();
    // FNV-1a（Rust のバージョンに依らず同じ値になる）
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in key.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    dir.join(format!("{h:016x}.cache"))
}

/// キャッシュの 1 件。
#[derive(Debug)]
pub struct Entry {
    /// サイズと更新日時。
    pub stamp: Stamp,
    /// 本文（frontmatter を含む全文）。
    pub text: String,
}

/// キャッシュファイルを読む。なければ・壊れていれば `None`。
pub fn read(path: &Path) -> Option<HashMap<String, Entry>> {
    let buf = std::fs::read(path).ok()?;
    decode(&buf)
}

fn decode(buf: &[u8]) -> Option<HashMap<String, Entry>> {
    let mut r = Reader { buf, pos: 0 };
    if r.take(8)? != MAGIC || r.u32()? != VERSION {
        return None;
    }
    let count = r.u64()? as usize;
    let mut map = HashMap::with_capacity(count.min(1 << 20));
    for _ in 0..count {
        let rel_len = r.u32()? as usize;
        let rel = std::str::from_utf8(r.take(rel_len)?).ok()?.to_string();
        let size = r.u64()?;
        let mtime_ns = r.u64()?;
        let text_len = r.u64()? as usize;
        let text = std::str::from_utf8(r.take(text_len)?).ok()?.to_string();
        map.insert(
            rel,
            Entry {
                stamp: Stamp { size, mtime_ns },
                text,
            },
        );
    }
    (r.pos == buf.len()).then_some(map)
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let s = self.buf.get(self.pos..end)?;
        self.pos = end;
        Some(s)
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
}

/// キャッシュファイルを書く（一時ファイルに書いてから置き換える）。
pub fn write<'a>(
    path: &Path,
    entries: impl ExactSizeIterator<Item = (&'a str, Stamp, &'a str)>,
) -> std::io::Result<()> {
    let mut buf = Vec::new();
    buf.extend_from_slice(MAGIC);
    buf.extend_from_slice(&VERSION.to_le_bytes());
    buf.extend_from_slice(&(entries.len() as u64).to_le_bytes());
    for (rel, stamp, text) in entries {
        buf.extend_from_slice(&(rel.len() as u32).to_le_bytes());
        buf.extend_from_slice(rel.as_bytes());
        buf.extend_from_slice(&stamp.size.to_le_bytes());
        buf.extend_from_slice(&stamp.mtime_ns.to_le_bytes());
        buf.extend_from_slice(&(text.len() as u64).to_le_bytes());
        buf.extend_from_slice(text.as_bytes());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&buf)?;
    }
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_corruption() {
        let dir = std::env::temp_dir().join(format!("okfast-cache-test-{}", std::process::id()));
        let path = dir.join("x.cache");
        let s = Stamp {
            size: 3,
            mtime_ns: 42,
        };
        write(&path, [("a/b.md", s, "本文"), ("c.md", s, "")].into_iter()).unwrap();
        let m = read(&path).unwrap();
        assert_eq!(m["a/b.md"].text, "本文");
        assert_eq!(m["a/b.md"].stamp, s);
        assert_eq!(m.len(), 2);

        let mut bytes = std::fs::read(&path).unwrap();
        bytes.truncate(bytes.len() - 1);
        assert!(decode(&bytes).is_none());
        bytes[8] = 99; // 形式のバージョン違い
        assert!(decode(&bytes).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cache_file_is_stable_per_root() {
        let d = Path::new("cache");
        assert_eq!(file_for(d, Path::new("x/y")), file_for(d, Path::new("x/y")));
        assert_ne!(file_for(d, Path::new("x/y")), file_for(d, Path::new("x/z")));
    }
}
