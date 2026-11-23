//! search（読む範囲）: 節の単位、長い節の切り出し、BM25 の順位、新しい記録、必ず読む範囲、JSON。

use okfast::aliases::Dictionary;
use okfast::bundle::Bundle;
use okfast::command::{self, Command, Context, SearchOptions};
use okfast::config::Config;
use serde_json::Value;
use std::path::{Path, PathBuf};

fn bundle_dir(name: &str, pages: &[(&str, &str)]) -> PathBuf {
    let root = std::env::temp_dir().join(format!("okfast-ranges-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for (rel, text) in pages {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    root
}

fn run(root: &Path, config: &str, terms: &[&str], opts: SearchOptions) -> (u8, String) {
    let bundle = Bundle::load(root).unwrap();
    let config = Config::parse(config, None).unwrap();
    let dict = Dictionary::build(&bundle, &config.aliases).unwrap();
    let cx = Context {
        bundle: &bundle,
        dict: &dict,
        config: &config,
        has_config: true,
    };
    let mut out = Vec::new();
    let cmd = Command::Search {
        terms: terms.iter().map(|s| s.to_string()).collect(),
        opts,
    };
    let code = command::run(&cmd, &cx, &mut out).unwrap();
    (code, String::from_utf8(out).unwrap())
}

fn json(root: &Path, config: &str, terms: &[&str], opts: SearchOptions) -> Value {
    let (_, out) = run(root, config, terms, SearchOptions { json: true, ..opts });
    serde_json::from_str(&out).unwrap()
}

fn spans(v: &Value) -> Vec<(String, u64, u64)> {
    v["ranges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["rel"].as_str().unwrap().to_string(),
                r["from"].as_u64().unwrap(),
                r["to"].as_u64().unwrap(),
            )
        })
        .collect()
}

const FM: &str = "---\ntype: Note\n---\n";

#[test]
fn section_is_the_unit() {
    // 答えの行（1.2 万円）には語がないが、同じ節に当たりがあるので範囲に入る
    let body = format!(
        "{FM}# 概要\nWidget の説明。\n\n## 価格\n価格は次のとおり。\n\n- 1 台 1.2 万円\n\n## 保守\n別の話。\n"
    );
    let root = bundle_dir("unit", &[("a.md", &body)]);
    let v = json(&root, "", &["価格"], SearchOptions::default());
    // ファイル上の行: 4 # 概要 / 7 ## 価格 / 10 - 1 台 1.2 万円 / 12 ## 保守
    assert_eq!(spans(&v), vec![("a.md".into(), 7, 10)]);
    let r = &v["ranges"][0];
    assert_eq!(r["heading"], "価格");
    assert_eq!(r["partial"], false);
    assert_eq!(r["hits"].as_array().unwrap().len(), 2);

    let (code, text) = run(&root, "", &["価格"], SearchOptions::default());
    assert_eq!(code, 0);
    assert!(text.contains("a.md:7-10［§価格］"), "{text}");
    let (code, _) = run(&root, "", &["存在しない"], SearchOptions::default());
    assert_eq!(code, 1);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn long_section_is_windowed() {
    let mut body = format!("{FM}# 長い節\n");
    for i in 0..60 {
        if i == 10 || i == 13 || i == 50 {
            body.push_str(&format!("行 {i} 価格\n"));
        } else {
            body.push_str(&format!("行 {i}\n"));
        }
    }
    let root = bundle_dir("long", &[("a.md", &body)]);
    let cfg = "[search.sections]\nmax_lines = 40\nwindow = 2\n";
    let v = json(
        &root,
        cfg,
        &["価格"],
        SearchOptions {
            per_page: 0,
            ..SearchOptions::default()
        },
    );
    // 見出しは 4 行目、「行 i」は 5 + i 行目。10 と 13 の切り出しはつながる
    let mut got = spans(&v);
    got.sort();
    assert_eq!(got, vec![("a.md".into(), 13, 20), ("a.md".into(), 53, 57)]);
    assert!(
        v["ranges"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["partial"] == true)
    );
    assert_eq!(v["ranges"][0]["section"], serde_json::json!([4, 64]));
    let (_, text) = run(&root, cfg, &["価格"], SearchOptions::default());
    assert!(text.contains("節 4-64 の一部"), "{text}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn rare_terms_weigh_more() {
    // 「部品」はどこにでもある、「保守」は 1 行だけ。保守の節が上に来る
    let mut common = format!("{FM}# 一覧\n");
    for i in 0..20 {
        common.push_str(&format!("部品 {i}\n"));
    }
    let root = bundle_dir(
        "bm25",
        &[
            ("a.md", &common),
            ("b.md", &format!("{FM}# 保守\n保守の契約は年単位。\n")),
        ],
    );
    let v = json(&root, "", &["部品", "保守"], SearchOptions::default());
    assert_eq!(v["ranges"][0]["rel"], "b.md");
    let s0 = v["ranges"][0]["score"].as_f64().unwrap();
    let s1 = v["ranges"][1]["score"].as_f64().unwrap();
    assert!(s0 > s1, "{s0} {s1}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn per_page_and_top() {
    let body = format!("{FM}# A\n価格 a\n# B\n価格 b\n# C\n価格 c\n");
    let root = bundle_dir(
        "limits",
        &[("a.md", &body), ("b.md", &format!("{FM}# X\n価格 x\n"))],
    );
    let v = json(&root, "", &["価格"], SearchOptions::default());
    let rels: Vec<String> = spans(&v).into_iter().map(|s| s.0).collect();
    assert_eq!(rels.iter().filter(|r| *r == "a.md").count(), 2);
    assert_eq!(v["rest"].as_array().unwrap().len(), 1);
    let v = json(
        &root,
        "",
        &["価格"],
        SearchOptions {
            top: 1,
            per_page: 0,
            ..SearchOptions::default()
        },
    );
    assert_eq!(v["ranges"].as_array().unwrap().len(), 1);
    assert_eq!(v["total_ranges"], 4);
    let _ = std::fs::remove_dir_all(&root);
}

const DATED: &str = "[search.recent]\ncount = 1\ndate_sources = [\"table_row\", \"heading\"]\n";

fn dated_bundle(name: &str) -> PathBuf {
    let mut log = format!("{FM}# 一覧\n");
    for i in 0..10 {
        log.push_str(&format!("価格 価格 価格 {i}\n"));
    }
    bundle_dir(
        name,
        &[
            ("a.md", &log),
            (
                "m.md",
                &format!(
                    "{FM}# 2026-04-01 定例\n価格の話が少し出た、ほかは長い議題ばかりだった\n\
# 2025-01-01 定例\n価格の話が少し出た、ほかは長い議題ばかりだった\n"
                ),
            ),
            (
                "t.md",
                &format!(
                    "{FM}# 記録\n| 日付 | 内容 |\n|---|---|\n| 2026-02-01 | 価格の改定、ほかにもいろいろな事情があって長い |\n"
                ),
            ),
        ],
    )
}

#[test]
fn recent_ranges_are_added() {
    let root = dated_bundle("recent");
    let v = json(
        &root,
        DATED,
        &["価格"],
        SearchOptions {
            top: 1,
            ..SearchOptions::default()
        },
    );
    let ranges = v["ranges"].as_array().unwrap();
    assert_eq!(ranges.len(), 2, "{v}");
    assert_eq!(ranges[0]["rel"], "a.md");
    assert_eq!(ranges[0]["date"], Value::Null);
    assert_eq!(ranges[1]["rel"], "m.md");
    assert_eq!(ranges[1]["recent"], true);
    assert_eq!(ranges[1]["date"], "2026-04-01");

    let v = json(
        &root,
        DATED,
        &["価格"],
        SearchOptions {
            since: Some("2026-01-15".into()),
            ..SearchOptions::default()
        },
    );
    let mut got: Vec<(String, String)> = v["ranges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["rel"].as_str().unwrap().into(),
                r["date"].as_str().unwrap().into(),
            )
        })
        .collect();
    got.sort();
    assert_eq!(
        got,
        vec![
            ("m.md".into(), "2026-04-01".into()),
            ("t.md".into(), "2026-02-01".into())
        ]
    );

    let v = json(
        &root,
        DATED,
        &["価格"],
        SearchOptions {
            sort_date: true,
            ..SearchOptions::default()
        },
    );
    let dates: Vec<&Value> = v["ranges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| &r["date"])
        .collect();
    assert_eq!(dates[0], "2026-04-01");
    assert_eq!(dates[1], "2026-02-01");
    assert!(
        v["ranges"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["recent"] == false)
    );

    // 日付の取り方がなければ足さない。--since はエラー
    let (_, text) = run(&root, "", &["価格"], SearchOptions::default());
    assert!(text.contains("新しい記録は足さない"), "{text}");
    let bundle = Bundle::load(&root).unwrap();
    let config = Config::default();
    let dict = Dictionary::build(&bundle, &config.aliases).unwrap();
    let cx = Context {
        bundle: &bundle,
        dict: &dict,
        config: &config,
        has_config: false,
    };
    let cmd = Command::Search {
        terms: vec!["価格".into()],
        opts: SearchOptions {
            since: Some("2026-01-01".into()),
            ..SearchOptions::default()
        },
    };
    assert!(command::run(&cmd, &cx, &mut Vec::new()).is_err());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn must_read_sections_and_pages() {
    let widget = "---\ntype: Product\ntitle: Widget\ncredibility: A\n---\n# 概要\nWidget の説明。\n## 未決事項\n- 保守の扱い\n## 価格\n1 万円\n";
    let root = bundle_dir(
        "must",
        &[
            ("notes/widget.md", widget),
            (
                "decisions/list.md",
                &format!("{FM}| 日付 | 決定 |\n|---|---|\n| 2026-03-02 | Widget を出す |\n"),
            ),
        ],
    );
    let cfg = "[search]\nexclude = [\"decisions/**\"]\nshow_keys = [\"credibility\"]\n\
[search.must_read]\nheadings = [\"未決\"]\npages = [{ path = \"decisions/*.md\", top = 1 }]\n";
    let v = json(&root, cfg, &["Widget"], SearchOptions::default());
    let ranges = v["ranges"].as_array().unwrap();
    // 題名に当たったページの「未決事項」の節（語の当たりなし）と、exclude にある決まったページ
    assert_eq!(ranges[0]["rel"], "notes/widget.md");
    assert_eq!(ranges[0]["heading"], "未決事項");
    assert_eq!(ranges[0]["must"], true);
    assert_eq!(ranges[0]["hits"].as_array().unwrap().len(), 0);
    assert_eq!(ranges[0]["meta"]["credibility"], "A");
    assert_eq!(ranges[1]["rel"], "decisions/list.md");
    assert_eq!(ranges[1]["must"], true);
    assert_eq!(ranges[2]["heading"], "概要");
    assert_eq!(ranges[2]["must"], false);
    // exclude のページは候補にはならない
    assert!(ranges[2..].iter().all(|r| r["rel"] != "decisions/list.md"));
    assert_eq!(v["excluded"], 1);

    let (_, text) = run(&root, cfg, &["Widget"], SearchOptions::default());
    assert!(text.contains("## 必ず読む範囲"), "{text}");
    assert!(
        text.contains("notes/widget.md:8-9［§未決事項｜credibility: A］"),
        "{text}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn min_chars_ignores_short_lines() {
    let root = bundle_dir(
        "minchars",
        &[(
            "a.md",
            &format!("{FM}# 一\n- 価格\n# 二\n価格の決め方は原価に利益を足す。\n# 価格\n本文\n"),
        )],
    );
    let cfg = "[search.sections]\nmin_chars = 6\n";
    let v = json(&root, cfg, &["価格"], SearchOptions::default());
    let headings: Vec<&str> = v["ranges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["heading"].as_str().unwrap())
        .collect();
    // 「- 価格」は短いので数えない。見出しの行は短くても数える
    assert!(!headings.contains(&"一"), "{headings:?}");
    assert!(
        headings.contains(&"二") && headings.contains(&"価格"),
        "{headings:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn config_rejects_unknown_date_source() {
    let e = Config::parse("[search.recent]\ndate_sources = [\"tablerow\"]\n", None).unwrap_err();
    assert!(e.to_string().contains("tablerow"), "{e}");
    assert!(
        Config::parse(
            "[search.recent]\ndate_sources = [\"frontmatter:date\"]\n",
            None
        )
        .is_ok()
    );
}
