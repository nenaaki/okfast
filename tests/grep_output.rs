//! grep の出し方: 順位、上限と省略の表示、前後の行、長い行の切り詰め。

use okfast::aliases::Dictionary;
use okfast::bundle::Bundle;
use okfast::command::{self, Command, Context, GrepOptions};
use okfast::config::Config;
use std::path::{Path, PathBuf};

fn bundle_dir(name: &str, pages: &[(&str, &str)]) -> PathBuf {
    let root = std::env::temp_dir().join(format!("okfast-grepout-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for (rel, text) in pages {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    root
}

fn grep(root: &Path, terms: &[&str], opts: GrepOptions) -> (u8, String) {
    let bundle = Bundle::load(root).unwrap();
    let config = Config::default();
    let dict = Dictionary::build(&bundle, &config.aliases).unwrap();
    let cx = Context {
        bundle: &bundle,
        dict: &dict,
        config: &config,
        has_config: false,
    };
    let mut out = Vec::new();
    let cmd = Command::Grep {
        terms: terms.iter().map(|s| s.to_string()).collect(),
        opts,
    };
    let code = command::run(&cmd, &cx, &mut out).unwrap();
    (code, String::from_utf8(out).unwrap())
}

fn page(title: &str, body: &str) -> String {
    format!("---\ntype: Note\ntitle: {title}\n---\n{body}")
}

#[test]
fn ranks_pages_by_terms_title_and_lines() {
    let many = "alpha\n".repeat(10);
    let root = bundle_dir(
        "rank",
        &[
            ("a/many.md", &page("Many", &many)),
            ("b/both.md", &page("Both", "alpha\nbeta\n")),
            ("c/titled.md", &page("Alpha の定義", "alpha\n")),
        ],
    );
    let (code, out) = grep(&root, &["alpha", "beta"], GrepOptions::default());
    assert_eq!(code, 0);
    let order: Vec<&str> = out
        .lines()
        .filter_map(|l| l.strip_prefix("# ページ: "))
        .map(|l| l.split('「').next().unwrap())
        .collect();
    // 語の種類が多い → 題名に当たる → 行が多い
    assert_eq!(order, vec!["b/both.md", "c/titled.md", "a/many.md"]);
    assert!(
        out.contains("# ページ: b/both.md「Both」 2 行・語 2/2"),
        "{out}"
    );
    assert!(
        out.contains("「Alpha の定義」 1 行・語 1/2・題名に当たり"),
        "{out}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn limits_lines_and_pages_and_reports_the_rest() {
    let many = "alpha\n".repeat(8);
    let root = bundle_dir(
        "limit",
        &[
            ("p1.md", &page("P1", &many)),
            ("p2.md", &page("P2", "alpha\nalpha\n")),
            ("p3.md", &page("P3", "alpha\n")),
        ],
    );
    let opts = GrepOptions {
        max_per_page: 3,
        max_pages: 2,
        ..GrepOptions::default()
    };
    let (_, out) = grep(&root, &["alpha"], opts);
    let hit_lines = out.lines().filter(|l| !l.starts_with('#')).count();
    assert_eq!(hit_lines, 3 + 2);
    assert!(out.contains("省いた行 5、行を出さないページ 1"), "{out}");
    // 省いた行は行番号で示す（frontmatter が 4 行なので本文は 5 行目から）
    assert!(
        out.contains("# （このページのほかの 5 行: 8, 9, 10, 11, 12）"),
        "{out}"
    );
    assert!(
        out.contains("# ほかに当たったページ（順位順・行数）: p3.md (1)"),
        "{out}"
    );

    // 上限なし
    let full = GrepOptions {
        max_per_page: 0,
        max_pages: 0,
        max_columns: 0,
        ..GrepOptions::default()
    };
    let (_, out) = grep(&root, &["alpha"], full);
    assert_eq!(out.lines().filter(|l| !l.starts_with('#')).count(), 11);
    assert!(!out.contains("# 表示:"), "{out}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn context_lines_and_clipping() {
    let root = bundle_dir(
        "ctx",
        &[(
            "n.md",
            &page(
                "N",
                "one\ntwo\nalpha\nfour\nfive\nsix\nseven\nalpha\nnine\n",
            ),
        )],
    );
    let opts = GrepOptions {
        context: 1,
        ..GrepOptions::default()
    };
    let (_, out) = grep(&root, &["alpha"], opts);
    let body: Vec<&str> = out.lines().filter(|l| !l.starts_with('#')).collect();
    assert_eq!(
        body,
        vec![
            "n.md-6-two",
            "n.md:7:alpha",
            "n.md-8-four",
            "--",
            "n.md-11-seven",
            "n.md:12:alpha",
            "n.md-13-nine",
        ]
    );

    let long = format!("alpha {}", "あ".repeat(50));
    let root2 = bundle_dir("clip", &[("l.md", &page("L", &long))]);
    let opts = GrepOptions {
        max_columns: 10,
        ..GrepOptions::default()
    };
    let (_, out) = grep(&root2, &["alpha"], opts);
    assert!(
        out.contains("l.md:5:alpha ああああ…（56 文字のうち 10 文字まで）"),
        "{out}"
    );
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&root2);
}

#[test]
fn total_budget_moves_the_rest_to_the_list() {
    let line = format!("alpha {}\n", "x".repeat(90));
    let pages: Vec<(String, String)> = (0..10)
        .map(|i| (format!("p{i}.md"), page(&format!("P{i}"), &line.repeat(3))))
        .collect();
    let refs: Vec<(&str, &str)> = pages
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let root = bundle_dir("budget", &refs);
    let opts = GrepOptions {
        max_chars: 700,
        ..GrepOptions::default()
    };
    let (_, out) = grep(&root, &["alpha"], opts);
    let shown = out.lines().filter(|l| l.starts_with("# ページ: ")).count();
    assert!((1..10).contains(&shown), "{out}");
    let body: usize = out
        .lines()
        .filter(|l| {
            !l.starts_with("# grep")
                && !l.starts_with("# 当たり")
                && !l.starts_with("# 表示")
                && !l.starts_with("# ほかに")
        })
        .map(|l| l.chars().count() + 1)
        .sum();
    assert!(body <= 700, "{body}\n{out}");
    assert!(
        out.contains("# ほかに当たったページ（順位順・行数）: "),
        "{out}"
    );

    // 1 ページ目は、上限より長くても少なくとも 1 行出す
    let tiny = GrepOptions {
        max_chars: 10,
        ..GrepOptions::default()
    };
    let (_, out) = grep(&root, &["alpha"], tiny);
    assert_eq!(
        out.lines().filter(|l| l.starts_with("p0.md:")).count(),
        1,
        "{out}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn pages_mode_lists_every_page_in_one_line() {
    let pages: Vec<(String, String)> = (0..40)
        .map(|i| {
            (
                format!("p{i:02}.md"),
                page(
                    &format!("P{i}"),
                    &format!("intro\nalpha {}\nalpha\n", "y".repeat(200)),
                ),
            )
        })
        .collect();
    let refs: Vec<(&str, &str)> = pages
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let root = bundle_dir("pages", &refs);
    let opts = GrepOptions {
        pages: true,
        ..GrepOptions::default()
    };
    let (_, out) = grep(&root, &["alpha"], opts);
    let lines: Vec<&str> = out.lines().filter(|l| !l.starts_with('#')).collect();
    assert_eq!(lines.len(), 40, "{out}");
    assert!(
        lines[0].starts_with("p00.md「P0」 2 行: 6: alpha yyy"),
        "{}",
        lines[0]
    );
    assert!(lines[0].ends_with('…'), "{}", lines[0]);

    // 文字数の目安を超えたら、残りは抜粋なしの一覧（ページは落とさない）
    let small = GrepOptions {
        pages: true,
        max_chars: 500,
        ..GrepOptions::default()
    };
    let (_, out) = grep(&root, &["alpha"], small);
    let shown = out.lines().filter(|l| !l.starts_with('#')).count();
    let rest = out
        .lines()
        .find_map(|l| l.strip_prefix("# 残りのページ（抜粋なし・順位順・行数）: "))
        .unwrap();
    assert_eq!(shown + rest.split(", ").count(), 40, "{out}");
    let _ = std::fs::remove_dir_all(&root);
}
