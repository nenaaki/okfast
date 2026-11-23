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
    assert_eq!(r["headings"], serde_json::json!(["概要", "価格"]));
    assert_eq!(r["partial"], false);
    assert_eq!(r["hits"].as_array().unwrap().len(), 2);

    let (code, text) = run(&root, "", &["価格"], SearchOptions::default());
    assert_eq!(code, 0);
    assert!(text.contains("a.md:7-10［§概要 > 価格］"), "{text}");
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
        text.contains("notes/widget.md:8-9［§概要 > 未決事項｜credibility: A］"),
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

#[test]
fn topic_pages_raise_scores_instead_of_coming_first() {
    // 「Widget」はありふれた語で、題名に当たるページは widget.md だけ。3 語すべてが当たる節が上に来る
    let mut widget = "---\ntype: Product\ntitle: Widget\n---\n".to_string();
    for i in 0..6 {
        widget.push_str(&format!("# 節 {i}\nWidget の説明 {i}。\n"));
    }
    let mut pages = vec![("widget.md".to_string(), widget)];
    for i in 0..10 {
        pages.push((
            format!("other{i}.md"),
            format!("{FM}# 雑記\nWidget の話 {i}。\n"),
        ));
    }
    pages.push((
        "deal.md".into(),
        format!("{FM}# 商談\nWidget の保守契約の更新期限は月末。\n"),
    ));
    let refs: Vec<(&str, &str)> = pages
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let root = bundle_dir("topic", &refs);
    let v = json(
        &root,
        "",
        &["Widget", "保守契約", "更新期限"],
        SearchOptions::default(),
    );
    assert_eq!(v["ranges"][0]["rel"], "deal.md", "{v}");
    assert_eq!(v["topics"][0]["rel"], "widget.md");
    let share = v["topics"][0]["topic"].as_f64().unwrap();
    assert!(share > 0.0 && share < 0.5, "{share}");
    // 題名の当たりは点を上げる: 本文が同じなら主題のページの範囲が上
    let o = json(&root, "", &["Widget"], SearchOptions::default());
    assert_eq!(o["ranges"][0]["rel"], "widget.md", "{o}");
    assert_eq!(o["ranges"][0]["topic"], 1.0);
    assert_eq!(o["topics"][0]["topic"], 1.0);

    // topic_keys で説明・タグのキーも主題の判定に使う
    let root2 = bundle_dir(
        "topickeys",
        &[
            (
                "a.md",
                "---\ntype: Note\ndescription: 保守契約のまとめ\ntags: [契約]\n---\n# 本文\n保守契約の話。\n",
            ),
            ("b.md", &format!("{FM}# 本文\n保守契約の話。\n")),
        ],
    );
    let v = json(&root2, "", &["保守契約"], SearchOptions::default());
    assert_eq!(v["topics"].as_array().unwrap().len(), 0, "{v}");
    let cfg = "[search]\ntopic_keys = [\"title\", \"description\", \"tags\"]\n";
    let v = json(&root2, cfg, &["保守契約"], SearchOptions::default());
    assert_eq!(v["topics"][0]["rel"], "a.md", "{v}");
    assert_eq!(v["ranges"][0]["rel"], "a.md");
    let v = json(&root2, cfg, &["契約"], SearchOptions::default());
    assert_eq!(v["topics"][0]["rel"], "a.md", "{v}");
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&root2);
}

#[test]
fn footnote_definitions_and_link_targets_are_not_hits() {
    let body = format!(
        "{FM}# 本文\n導入の時期は未定[^m1]。\n\n# 関連\n- [議事録](/meetings/price-review.md)\n- [価格表](/price.md)\n\n\
[^m1]: meetings/2026-01-15 価格の検討会.md\n"
    );
    let root = bundle_dir("footnote", &[("a.md", &body)]);
    let v = json(&root, "", &["価格"], SearchOptions::default());
    let hits: Vec<u64> = v["ranges"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|r| r["hits"].as_array().unwrap().iter())
        .map(|h| h["line"].as_u64().unwrap())
        .collect();
    // 9 行目（[価格表]）の表示の文字だけが当たる。脚注の定義（11 行目）は当たらない
    assert_eq!(hits, vec![9], "{v}");
    // 行き先（/price.md・price-review）だけに当たる語は数えない
    let v = json(&root, "", &["price"], SearchOptions::default());
    assert_eq!(v["ranges"].as_array().unwrap().len(), 0, "{v}");
    // skip_footnotes = false なら脚注の定義も数える
    let v = json(
        &root,
        "[search.sections]\nskip_footnotes = false\n",
        &["価格"],
        SearchOptions::default(),
    );
    assert_eq!(v["total_lines"], 2, "{v}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn decay_rewards_sections_with_many_hits() {
    // a.md は語が 3 行に当たる節、b.md は 1 行だけ当たる短い節（1 行の点は b.md が高い）
    let root = bundle_dir(
        "decay",
        &[
            (
                "a.md",
                &format!(
                    "{FM}# 手順\n変更の申請を出す。\n\
変更の承認を受ける。\n変更を反映する。\n"
                ),
            ),
            ("b.md", &format!("{FM}# 雑記\n変更あり\n")),
        ],
    );
    let v = json(&root, "", &["変更"], SearchOptions::default());
    assert_eq!(v["ranges"][0]["rel"], "a.md", "{v}");
    let v = json(
        &root,
        "[search.sections]\nscore = \"max\"\n",
        &["変更"],
        SearchOptions::default(),
    );
    assert_eq!(v["ranges"][0]["rel"], "b.md", "{v}");
    assert!(Config::parse("[search.sections]\nscore = \"sum\"\n", None).is_err());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn must_read_page_window() {
    let mut log = format!("{FM}# 決定\n| 日付 | 決定 |\n|---|---|\n");
    for i in 0..8 {
        log.push_str(&format!("| 2026-03-0{} | Widget の決定 {i} |\n", i + 1));
    }
    let root = bundle_dir("mustwindow", &[("decisions/list.md", &log)]);
    let cfg = "[search.must_read]\npages = [{ path = \"decisions/*.md\", top = 3, window = 0 }]\n";
    let v = json(&root, cfg, &["Widget"], SearchOptions::default());
    let must: Vec<(u64, u64)> = v["ranges"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["must"] == true)
        .map(|r| (r["from"].as_u64().unwrap(), r["to"].as_u64().unwrap()))
        .collect();
    // 隣り合う行もつながず、1 行ずつ 3 件
    assert_eq!(must.len(), 3, "{v}");
    assert!(must.iter().all(|(a, b)| a == b), "{must:?}");
    // window を省くと [search.sections] の切り方（短い節は節全体）
    let cfg = "[search.must_read]\npages = [{ path = \"decisions/*.md\", top = 3 }]\n";
    let v = json(&root, cfg, &["Widget"], SearchOptions::default());
    assert_eq!(v["ranges"][0]["from"], 4);
    assert_eq!(v["ranges"][0]["to"], 14);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn path_limits_pages_and_includes_excluded() {
    let root = bundle_dir(
        "path",
        &[
            ("notes/a.md", &format!("{FM}# A\n価格 a\n")),
            ("meetings/m.md", &format!("{FM}# M\n価格 m\n")),
            ("transcripts/t.md", &format!("{FM}# T\n価格 t\n")),
        ],
    );
    let cfg = "[search]\nexclude = [\"transcripts/**\"]\n";
    let rels = |v: &Value| {
        let mut r: Vec<String> = spans(v).into_iter().map(|s| s.0).collect();
        r.sort();
        r
    };
    let v = json(&root, cfg, &["価格"], SearchOptions::default());
    assert_eq!(rels(&v), vec!["meetings/m.md", "notes/a.md"]);
    let v = json(
        &root,
        cfg,
        &["価格"],
        SearchOptions {
            paths: vec!["meetings/**".into(), "transcripts/**".into()],
            ..SearchOptions::default()
        },
    );
    assert_eq!(rels(&v), vec!["meetings/m.md", "transcripts/t.md"]);
    assert_eq!(v["searched"], 2);
    assert_eq!(
        v["paths"],
        serde_json::json!(["meetings/**", "transcripts/**"])
    );
    let (_, text) = run(
        &root,
        cfg,
        &["価格"],
        SearchOptions {
            paths: vec!["meetings/**".into()],
            ..SearchOptions::default()
        },
    );
    assert!(text.contains("--path meetings/**"), "{text}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn year_less_table_dates_use_generated_at() {
    let page = "---\ntype: Log\ngenerated: { by: human:someone, at: 2027-01-10T09:00:00Z }\n---\n\
# 記録\n| 日 | 内容 |\n|---|---|\n| 12-20 | 価格の話、ほかにも長い説明が続いている |\n\
| 01-05 | 価格の話、ほかにも長い説明が続いている |\n";
    let root = bundle_dir("yearless", &[("history.md", page)]);
    let cfg = "[search.recent]\ndate_sources = [\"table_row\"]\n";
    let v = json(&root, cfg, &["価格"], SearchOptions::default());
    let dates: Vec<&str> = v["ranges"][0]["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["date"].as_str().unwrap())
        .collect();
    assert_eq!(dates, vec!["2026-12-20", "2027-01-05"], "{v}");
    assert_eq!(v["ranges"][0]["date"], "2027-01-05");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn term_counts_raise_lines_that_repeat_terms() {
    // 担当の一覧の行には二人の名前と WG 名が並ぶ。会議の行は名前を 1 回出すだけ（同じ長さ）
    let root = bundle_dir(
        "tf",
        &[
            (
                "people.md",
                &format!("{FM}# 担当\n| 山田・佐藤 | 生産 WG、山田が主担当 |\n"),
            ),
            (
                "meeting.md",
                &format!("{FM}# 議事\n| 報告 | 山田から生産の進み具合 |\n"),
            ),
        ],
    );
    let terms = ["山田|佐藤", "生産"];
    let v = json(&root, "", &terms, SearchOptions::default());
    assert_eq!(v["ranges"][0]["rel"], "people.md", "{v}");
    let s0 = v["ranges"][0]["score"].as_f64().unwrap();
    let s1 = v["ranges"][1]["score"].as_f64().unwrap();
    assert!(s0 > s1, "{s0} {s1}");
    let gap_count = s0 - s1;
    // binary なら回数を数えないので、差は長さの違いの分だけになる
    let v = json(
        &root,
        "[search.sections]\ntf = \"binary\"\n",
        &terms,
        SearchOptions::default(),
    );
    let gap_binary =
        v["ranges"][0]["score"].as_f64().unwrap() - v["ranges"][1]["score"].as_f64().unwrap();
    assert!(
        gap_count > gap_binary.abs() * 2.0,
        "{gap_count} {gap_binary}"
    );
    // リンクの行き先の出現は数えない: 行き先だけが違う同じ長さの行は同じ点
    let root2 = bundle_dir(
        "tflink",
        &[
            (
                "a.md",
                &format!(
                    "{FM}# A
山田の担当 [山田](/p/山田.md)
"
                ),
            ),
            (
                "b.md",
                &format!(
                    "{FM}# B
山田の担当 [山田](/p/田中.md)
"
                ),
            ),
        ],
    );
    let v = json(&root2, "", &["山田"], SearchOptions::default());
    let scores: Vec<f64> = v["ranges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["score"].as_f64().unwrap())
        .collect();
    assert_eq!(scores.len(), 2, "{v}");
    assert!((scores[0] - scores[1]).abs() < 1e-9, "{scores:?}");
    for bad in ["k1 = -1.0", "b = 1.5"] {
        assert!(Config::parse(&format!("[search.sections]\n{bad}\n"), None).is_err());
    }
    assert!(Config::parse("[search.sections]\ntf = \"often\"\n", None).is_err());
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&root2);
}

#[test]
fn recent_comes_from_the_pool_below_the_top() {
    // 関連度の高い範囲が並び、いちばん下に日付だけ新しい「関連」の節がある
    let mut pages: Vec<(String, String)> = Vec::new();
    for i in 0..6 {
        pages.push((
            format!("p{i}.md"),
            format!(
                "{FM}# 記録 2026-01-0{}\n価格の改定 価格の決め方 {i}\n",
                i + 1
            ),
        ));
    }
    pages.push((
        "plan.md".into(),
        format!(
            "{FM}# 2027-06-14 予定\nこの回では、ほかにも多くの議題を扱う予定で、価格の話も少し出るかもしれない。\n"
        ),
    ));
    let refs: Vec<(&str, &str)> = pages
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let root = bundle_dir("pool", &refs);
    let recent = |cfg: &str| -> Vec<String> {
        let v = json(
            &root,
            cfg,
            &["価格"],
            SearchOptions {
                top: 2,
                ..SearchOptions::default()
            },
        );
        v["ranges"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["recent"] == true)
            .map(|r| r["rel"].as_str().unwrap().to_string())
            .collect()
    };
    let base = "[search.recent]\ncount = 1\ndate_sources = [\"heading\"]\n";
    // 上位の次の 2 範囲（p? の 2 つ）から選ぶので、いちばん下の plan.md は選ばない
    let got = recent(&format!("{base}pool = 2\n"));
    assert_eq!(got.len(), 1);
    assert_ne!(got[0], "plan.md");
    // pool = 0 は全範囲から（従来の動き）
    assert_eq!(recent(&format!("{base}pool = 0\n")), vec!["plan.md"]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn table_row_uses_only_a_leading_date() {
    let page = format!(
        "{FM}# 一覧\n| 内容 | 備考 |\n|---|---|\n| 2026-03-02 価格の改定、ほかにも長い説明 | 済 |\n\
| 価格の会議、第 6 回は 2027/6/14、ほかにも長い説明 | 予定 |\n"
    );
    let root = bundle_dir("celldate", &[("t.md", &page)]);
    let cfg = "[search.recent]\ndate_sources = [\"table_row\"]\n";
    let v = json(&root, cfg, &["価格"], SearchOptions::default());
    let dates: Vec<Value> = v["ranges"][0]["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["date"].clone())
        .collect();
    assert_eq!(dates, vec![Value::from("2026-03-02"), Value::Null], "{v}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn body_returns_range_lines() {
    // ファイル上の行: 4 # 概要 / 7 ## 価格 / 8 価格は次のとおり。 / 10 - 1 台 1.2 万円 / 12 ## 保守
    let body = format!(
        "{FM}# 概要\nWidget の説明。\n\n## 価格\n価格は次のとおり。\n\n- 1 台 1.2 万円\n\n## 保守\n別の話。価格はなし\n"
    );
    let root = bundle_dir("body", &[("a.md", &body)]);
    let opts = SearchOptions {
        body: 1,
        ..SearchOptions::default()
    };
    let v = json(&root, "", &["価格"], opts.clone());
    let r = &v["ranges"];
    assert_eq!(r[0]["from"], 7);
    assert_eq!(
        r[0]["body"],
        serde_json::json!([
            {"line": 7, "text": "## 価格"},
            {"line": 8, "text": "価格は次のとおり。"},
            {"line": 9, "text": ""},
            {"line": 10, "text": "- 1 台 1.2 万円"},
        ])
    );
    assert_eq!(r[0]["body_over_limit"], false);
    // 上位 1 範囲だけ
    assert_eq!(r[1]["body"], Value::Null);
    assert_eq!(r[1]["body_over_limit"], false);

    let (_, text) = run(&root, "", &["価格"], opts.clone());
    assert!(
        text.contains(
            "a.md:7-10［§概要 > 価格］\n  7: ## 価格\n  8: 価格は次のとおり。\n  9: \n  10: - 1 台 1.2 万円\n"
        ),
        "{text}"
    );
    assert!(
        text.contains("# 本文を出していない範囲は Read で読む"),
        "{text}"
    );

    // 合計行数の上限を超えた範囲から下は抜粋だけ
    let opts = SearchOptions {
        body: 2,
        body_lines: 3,
        ..SearchOptions::default()
    };
    let v = json(&root, "", &["価格"], opts.clone());
    assert_eq!(v["ranges"][0]["body"], Value::Null);
    assert_eq!(v["ranges"][0]["body_over_limit"], true);
    assert_eq!(v["ranges"][1]["body_over_limit"], true);
    let (_, text) = run(&root, "", &["価格"], opts);
    assert!(
        text.contains("（本文は --body-lines の上限を超えるため出さない。Read で読む）"),
        "{text}"
    );
    assert!(text.contains("# 範囲は Read で読む"), "{text}");

    // すべて返せば Read の案内はしない
    let (_, text) = run(
        &root,
        "",
        &["価格"],
        SearchOptions {
            body: 5,
            ..SearchOptions::default()
        },
    );
    assert!(text.contains("# 範囲の本文はすべて上に出した"), "{text}");
    assert!(!text.contains("Read で読む"), "{text}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn body_of_partial_range_is_only_the_part() {
    let mut body = format!("{FM}# 長い節\n");
    for i in 0..60 {
        body.push_str(&format!("行 {i}\n"));
    }
    body.push_str("価格の行\n");
    let root = bundle_dir("body-partial", &[("a.md", &body)]);
    let v = json(
        &root,
        "[search.sections]\nwindow = 1\n",
        &["価格"],
        SearchOptions {
            body: 1,
            ..SearchOptions::default()
        },
    );
    let r = &v["ranges"][0];
    assert_eq!(r["partial"], true);
    let lines: Vec<u64> = r["body"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["line"].as_u64().unwrap())
        .collect();
    let (from, to) = (r["from"].as_u64().unwrap(), r["to"].as_u64().unwrap());
    assert_eq!(lines, (from..=to).collect::<Vec<_>>());
    assert_eq!(
        r["body"].as_array().unwrap().last().unwrap()["text"],
        "価格の行"
    );
    let _ = std::fs::remove_dir_all(&root);
}

const TIMELINE_CFG: &str = r#"
[search.recent]
date_sources = ["table_row", "heading"]

[search.timeline]
kinds = [
  { name = "decision",   pattern = "【決定】" },
  { name = "conclusion", pattern = "【結論】" },
]
superseded = ["【結論・{date}に変更】"]
"#;

fn timeline_bundle(name: &str) -> PathBuf {
    bundle_dir(
        name,
        &[
            (
                "records.md",
                "---\ntype: Log\ngenerated: { by: human:someone, at: 2026-11-23T09:00:00Z }\n---\n\
# 価格の記録\n\
| 日 | 内容 |\n\
|---|---|\n\
| 10/1 | 【結論・11/20に変更】据え置き |\n\
| 10/15 | 価格の資料を共有した |\n\
| 11/20 | 【結論】5% 上げる |\n\
| 11/21 | 【決定】価格の告知は 12 月 |\n",
            ),
            (
                "m.md",
                &format!(
                    "{FM}# 2026-09-01 定例\n価格の話【決定】見積もりは月末\n\n# 日付なし\n価格【決定】日付のない行\n"
                ),
            ),
            (
                "other.md",
                &format!("{FM}# 2026-12-01 別件\n【決定】価格と関係ない話\n"),
            ),
        ],
    )
}

#[test]
fn timeline_lists_dated_lines_of_returned_ranges() {
    let root = timeline_bundle("timeline");
    // other.md は返す範囲に入らないよう --path で外す
    let opts = SearchOptions {
        paths: vec!["records.md".into(), "m.md".into()],
        ..SearchOptions::default()
    };
    let v = json(&root, TIMELINE_CFG, &["価格"], opts.clone());
    let tl = v["timeline"].as_array().unwrap();
    let rows: Vec<(String, String, u64, Value)> = tl
        .iter()
        .map(|e| {
            (
                e["date"].as_str().unwrap().to_string(),
                e["rel"].as_str().unwrap().to_string(),
                e["line"].as_u64().unwrap(),
                e["kind"].clone(),
            )
        })
        .collect();
    // ファイル上の行: records.md 5 見出し / 8 10/1 / 9 10/15 / 10 11/20 / 11 11/21、m.md 4 見出し / 5 本文
    // 既定では語に当たった行だけ。ただし変更の印のある行（8）は当たっていなくても出す。語のない 10 は出さない
    assert_eq!(
        rows,
        vec![
            (
                "2026-11-21".into(),
                "records.md".into(),
                11,
                "decision".into()
            ),
            ("2026-10-01".into(), "records.md".into(), 8, Value::Null),
            ("2026-09-01".into(), "m.md".into(), 5, "decision".into()),
        ]
    );
    // 変更の印は、変更先の日付の行を指す（印の行には種類がないので、種類の付いた行）。変更先は一覧に
    // 出さない行（語に当たっていない行）からも探す
    assert_eq!(tl[1]["superseded"], true);
    assert_eq!(
        tl[1]["superseded_by"],
        serde_json::json!({"date": "2026-11-20", "rel": "records.md", "line": 10})
    );
    assert_eq!(tl[0]["superseded"], false);
    assert_eq!(tl[0]["superseded_by"], Value::Null);

    let (_, text) = run(&root, TIMELINE_CFG, &["価格"], opts.clone());
    assert!(
        text.contains(
            "## 時系列（返した範囲の日付つきの行。新しい順）\n2026-11-21｜records.md:11｜decision｜| 11/21 | 【決定】価格の告知は 12 月 |\n"
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "2026-10-01｜records.md:8｜-｜| 10/1 | 【結論・11/20に変更】据え置き |｜→ 2026-11-20 に変更（records.md:10）\n"
        ),
        "{text}"
    );
    // 返した範囲の外（other.md）は入らない
    assert!(!text.contains("関係ない話"), "{text}");

    // 上限
    let v = json(
        &root,
        TIMELINE_CFG,
        &["価格"],
        SearchOptions {
            timeline: Some(2),
            ..opts.clone()
        },
    );
    assert_eq!(v["timeline"].as_array().unwrap().len(), 2);
    let (_, text) = run(
        &root,
        TIMELINE_CFG,
        &["価格"],
        SearchOptions {
            timeline: Some(0),
            ..opts.clone()
        },
    );
    assert!(!text.contains("## 時系列"), "{text}");

    let lines_of = |cfg: &str| -> Vec<(String, u64)> {
        json(&root, cfg, &["価格"], opts.clone())["timeline"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| {
                (
                    e["rel"].as_str().unwrap().to_string(),
                    e["line"].as_u64().unwrap(),
                )
            })
            .collect()
    };
    // hits_only = false なら語に当たっていない行も出す
    let lines = lines_of(&format!("{TIMELINE_CFG}hits_only = false\n"));
    assert!(lines.contains(&("records.md".into(), 10)), "{lines:?}");
    assert!(!lines.contains(&("records.md".into(), 9)), "{lines:?}");

    // kinds_only = false なら種類のない行も（既定では語に当たった行だけ）
    let lines = lines_of(&format!("{TIMELINE_CFG}kinds_only = false\n"));
    assert!(lines.contains(&("records.md".into(), 9)), "{lines:?}");
    assert!(!lines.contains(&("m.md".into(), 4)), "{lines:?}");

    // 両方 false なら日付の取れる行すべて（区切りの行・空行は除く）
    let lines = lines_of(&format!(
        "{TIMELINE_CFG}kinds_only = false\nhits_only = false\n"
    ));
    assert!(lines.contains(&("records.md".into(), 9)), "{lines:?}");
    assert!(lines.contains(&("m.md".into(), 4)), "{lines:?}");
    assert!(
        !lines.iter().any(|(r, l)| r == "records.md" && *l == 7),
        "{lines:?}"
    );
    assert!(
        !lines.iter().any(|(r, l)| r == "m.md" && *l == 6),
        "{lines:?}"
    );

    // 種類も印も宣言しなければ出さない。--timeline を指定したら理由を示す
    let bare = "[search.recent]\ndate_sources = [\"table_row\"]\n";
    let v = json(&root, bare, &["価格"], opts.clone());
    assert_eq!(v["timeline"], serde_json::json!([]));
    let (_, text) = run(
        &root,
        bare,
        &["価格"],
        SearchOptions {
            timeline: Some(10),
            ..opts.clone()
        },
    );
    assert!(
        text.contains("時系列の一覧は出さない（[search.timeline]"),
        "{text}"
    );
    let (_, text) = run(
        &root,
        "",
        &["価格"],
        SearchOptions {
            timeline: Some(10),
            ..opts
        },
    );
    assert!(
        text.contains("時系列の一覧は出さない（[search.recent] date_sources）"),
        "{text}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn timeline_config_is_validated() {
    for bad in [
        "[search.timeline]\nkinds = [{ name = \"\", pattern = \"x\" }]\n",
        "[search.timeline]\nkinds = [{ name = \"a\", pattern = \"\" }]\n",
        "[search.timeline]\nsuperseded = [\"{date}\"]\n",
        "[search.timeline]\nsuperseded = [\"{date}・{date}\"]\n",
        "[search.timeline]\nunknown = 1\n",
    ] {
        assert!(Config::parse(bad, None).is_err(), "{bad}");
    }
    assert!(Config::parse(TIMELINE_CFG, None).is_ok());
}
