//! キャッシュの当たり・読み直し・削除の結合テスト。

use okfast::bundle::{Bundle, CacheMode};
use std::fs;
use std::path::{Path, PathBuf};

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("okfast-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

#[test]
fn reuses_and_invalidates() {
    let root = temp("cache-bundle");
    let cache = temp("cache-dir");
    let mode = CacheMode::Dir(cache.clone());
    write(&root, "a.md", "---\ntype: X\n---\nalpha\n");
    write(&root, "sub/b.md", "---\ntype: Y\n---\nbeta\n");
    write(&root, "index.md", "# index\n");

    // 初回: すべて読み、キャッシュを書く
    let (b, s) = Bundle::load_with(&root, &mode).unwrap();
    assert_eq!((s.cached, s.read, s.removed), (0, 2, 0));
    assert!(s.cache_written);
    assert_eq!(b.pages.len(), 2);

    // 2 回目: すべてキャッシュから。書き直さない
    let (b, s) = Bundle::load_with(&root, &mode).unwrap();
    assert_eq!((s.cached, s.read), (2, 0));
    assert!(!s.cache_written);
    assert_eq!(b.page("sub/b.md").unwrap().body(), "beta\n");
    assert_eq!(
        b.page("a.md").unwrap().frontmatter().okf_type().as_deref(),
        Some("X")
    );

    // 変更（サイズが変わる）・追加・削除
    write(&root, "a.md", "---\ntype: X\n---\nalpha changed\n");
    write(&root, "c.md", "---\ntype: Z\n---\ngamma\n");
    fs::remove_file(root.join("sub/b.md")).unwrap();
    let (b, s) = Bundle::load_with(&root, &mode).unwrap();
    assert_eq!((s.cached, s.read, s.removed), (0, 2, 1));
    assert!(s.cache_written);
    assert_eq!(b.page("a.md").unwrap().body(), "alpha changed\n");
    assert!(b.page("sub/b.md").is_none());

    // 壊れたキャッシュは黙って作り直す
    fs::write(s.cache_file.as_ref().unwrap(), b"garbage").unwrap();
    let (_, s) = Bundle::load_with(&root, &mode).unwrap();
    assert_eq!((s.cached, s.read), (0, 2));
    assert!(s.cache_written);

    // キャッシュなし
    let (_, s) = Bundle::load_with(&root, &CacheMode::Off).unwrap();
    assert_eq!((s.cached, s.read), (0, 2));
    assert!(s.cache_file.is_none());

    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&cache);
}
