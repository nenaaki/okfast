//! `okfast config check / init / reference` の結合テスト（tests/fixtures/bundle）。

use okfast::aliases::Dictionary;
use okfast::bundle::Bundle;
use okfast::check::{Level, Report, check};
use okfast::config::{Config, REFERENCE};
use okfast::init::draft;
use std::path::{Path, PathBuf};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bundle")
}

fn warnings(r: &Report) -> Vec<String> {
    r.sections
        .iter()
        .flat_map(|s| &s.findings)
        .filter(|f| f.level == Level::Warn)
        .map(|f| f.message.clone())
        .collect()
}

fn all_messages(r: &Report) -> String {
    r.sections
        .iter()
        .flat_map(|s| &s.findings)
        .map(|f| f.message.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn check_fixture_config() {
    let root = fixture();
    let (cfg, path) = Config::discover(&root, None).unwrap();
    let bundle = Bundle::load(&root).unwrap();
    let r = check(&bundle, &cfg, path.as_deref()).unwrap();
    // 試験用バンドルでは、credibility: D の表を trust で除いていることだけが警告になる
    let w = warnings(&r);
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].contains("aliases.trust"));
    let all = all_messages(&r);
    assert!(all.contains("概念 2"));
    assert!(all.contains("短い語 1"));
}

#[test]
fn check_reports_mistakes_with_hints() {
    let cfg = Config::parse(
        r#"
[[aliases.table]]
pages   = { path = "glossary/name.md" }
concept = "Concept"
names   = "Name"
[[aliases.table.conditional]]
column = "Misheard"
when   = { path = "transcripts/**" }

[[aliases.table]]
pages = { type = "Glossary Table" }
names = "Alias"

[[aliases.frontmatter]]
keys = ["alias"]

[search]
exclude = ["nothing/**"]
"#,
        None,
    )
    .unwrap();
    let bundle = Bundle::load(&fixture()).unwrap();
    let r = check(&bundle, &cfg, None).unwrap();
    let all = all_messages(&r);
    for expected in [
        "設定ファイルがない",
        "当たるページがない",
        "「Name」に似た見出し: Names",
        "when（path=transcripts/**）に当たるページがない",
        "列「Alias」を持つ表がない",
        "Concept | Names | Misheard | Topic",
        "キー alias を持つページがない",
        "aliases(1)",
        "辞書が空",
        "exclude「nothing/**」に当たるページがない",
    ] {
        assert!(all.contains(expected), "「{expected}」がない:\n{all}");
    }
}

#[test]
fn check_without_sources() {
    let bundle = Bundle::load(&fixture()).unwrap();
    let r = check(&bundle, &Config::default(), None).unwrap();
    let all = all_messages(&r);
    assert!(all.contains("okfast config init"));
    assert!(all.contains("取り出し元"));
}

fn contents(r: &Report) -> Vec<String> {
    r.sections
        .iter()
        .flat_map(|s| &s.findings)
        .filter(|f| f.level == Level::Content)
        .map(|f| f.message.clone())
        .collect()
}

#[test]
fn init_draft_is_valid_and_finds_sources() {
    let bundle = Bundle::load(&fixture()).unwrap();
    let text = draft(&bundle);
    let cfg = Config::parse(&text, None).expect("下書きが設定として読めない");
    assert_eq!(cfg.aliases.table.len(), 3, "{text}");

    // 概念名が呼び名の列にも書かれている表: concept と names を分ける
    let t = cfg
        .aliases
        .table
        .iter()
        .find(|t| t.conditional.len() == 1)
        .unwrap();
    assert_eq!(t.names_columns(), vec!["Names"]);
    assert_eq!(t.concept.as_deref(), Some("Concept"));
    assert_eq!(t.boost_columns(), vec!["Topic"]);
    assert_eq!(t.conditional[0].column, "Misheard");

    // 概念名が呼び名の列に書かれていない表: 概念名の列も呼び名にし、概念名のリンクを主題のページにする
    let t = cfg
        .aliases
        .table
        .iter()
        .find(|t| t.names_columns().len() == 2)
        .unwrap();
    assert_eq!(t.names_columns(), vec!["用語", "別表記"]);
    assert!(t.concept.is_none());
    assert_eq!(t.boost_columns(), vec!["用語"]);
    assert!(text.contains("# strip_notes"));

    assert_eq!(cfg.aliases.frontmatter[0].keys, vec!["aliases"]);
    assert!(
        cfg.search
            .exclude
            .contains(&"glossary/names.md".to_string())
    );

    // 下書きのまま確かめると、設定の警告はなく、2 つの表にある同じ語が辞書の中身の所見になる
    let r = check(&bundle, &cfg, None).unwrap();
    let w: Vec<String> = warnings(&r)
        .into_iter()
        .filter(|m| !m.contains("設定ファイルがない"))
        .collect();
    assert!(w.is_empty(), "{w:?}");
    let c = contents(&r);
    assert!(
        c.iter()
            .any(|m| m.contains("複数の概念にある語") && m.contains("「Widget Pro」")),
        "{c:?}"
    );
}

#[test]
fn multi_column_names_links_placeholders_and_notes() {
    let bundle = Bundle::load(&fixture()).unwrap();
    let cfg = |notes: &str| {
        Config::parse(
            &format!(
                r#"
[[aliases.table]]
pages = {{ path = "vocab/terms.md" }}
names = ["用語", "別表記"]
boost = "用語"
{notes}
"#
            ),
            None,
        )
        .unwrap()
    };
    let with_notes = cfg(r#"strip_notes = ["（"]"#);
    let dict = Dictionary::build(&bundle, &with_notes.aliases).unwrap();
    // 別表記から、リンクになっている用語へ戻る。リンク先（相対パス）は主題のページになる
    let e = dict.expand(&["スプロケット".to_string()]);
    assert_eq!(e.added, vec!["Sprocket", "SPK"]);
    assert_eq!(e.boost, vec!["notes/sprocket.md"]);
    // 「—」は語にしない
    let e = dict.expand(&["Gizmo".to_string()]);
    assert!(!e.is_empty());
    assert!(e.added.is_empty());
    assert!(dict.expand(&["—".to_string()]).is_empty());

    // strip_notes がなければ注記ごと 1 つの語になる
    let dict = Dictionary::build(&bundle, &cfg("").aliases).unwrap();
    let e = dict.expand(&["Sprocket".to_string()]);
    assert_eq!(e.added, vec!["スプロケット", "SPK（略称）"]);
}

#[test]
fn check_flags_long_names_as_content() {
    let bundle = Bundle::load(&fixture()).unwrap();
    let cfg = Config::parse(
        r#"
[[aliases.table]]
pages = { path = "vocab/terms.md" }
names = ["意味", "用語"]
"#,
        None,
    )
    .unwrap();
    let r = check(&bundle, &cfg, None).unwrap();
    // 長い語はないので所見なし。「意味」の列の語も呼び名になる
    assert!(contents(&r).iter().all(|m| !m.contains("文字を超える")));
    assert!(all_messages(&r).contains("歯車"));
}

#[test]
fn reference_documents_every_key() {
    // 設定のキーを足したら、docs/config.md にも書く
    for key in [
        "[[aliases.table]]",
        "[[aliases.table.conditional]]",
        "[[aliases.frontmatter]]",
        "[aliases.match]",
        "[aliases.trust]",
        "[search]",
        "`pages`",
        "`names`",
        "`concept`",
        "`split`",
        "`boost`",
        "`column`",
        "`when`",
        "`keys`",
        "`include_title`",
        "`strip_notes`",
        "`short_max`",
        "`key`",
        "`allow`",
        "`exclude`",
        "`show_keys`",
        "`topic_keys`",
        "`score`",
        "`skip_footnotes`",
        "generated.at",
        "[search.sections]",
        "[search.recent]",
        "[search.must_read]",
        "`max_lines`",
        "`window`",
        "`min_chars`",
        "`count`",
        "`date_sources`",
        "`table_columns`",
        "`table_row`",
        "`footnote_source`",
        "`heading`",
        "frontmatter:<キー>",
        "`topics`",
        "`headings`",
        "`top`",
        "config check",
        "config init",
    ] {
        assert!(REFERENCE.contains(key), "docs/config.md に {key} がない");
    }
}

#[test]
fn search_settings_are_drafted_and_checked() {
    let root = std::env::temp_dir().join(format!("okfast-cfg-search-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("log.md"),
        "---\ntype: Log\n---\n予約名なのでページにならない\n",
    )
    .unwrap();
    std::fs::write(
        root.join("meetings.md"),
        "---\ntype: Note\nupdated: 2026-05-01\n---\n# 2026-04-01 定例\n## 未決\n- 保守\n| 日付 | 内容 |\n|---|---|\n| 2026/3/2 | 改定 |\n",
    )
    .unwrap();
    let bundle = Bundle::load(&root).unwrap();

    // 下書きは、バンドルにある日付の書き方だけを提案し、そのまま読める
    let d = draft(&bundle);
    assert!(
        d.contains(r#"date_sources = ["table_row", "frontmatter:updated", "heading"]"#),
        "{d}"
    );
    assert!(d.contains("# 根拠: 表の 1 列目が日付の行 1"), "{d}");
    Config::parse(&d, None).unwrap();

    let cfg = Config::parse(
        r#"
[search]
show_keys = ["credibility"]
[search.recent]
date_sources  = ["table_row", "footnote_source"]
table_columns = ["日にち"]
[search.must_read]
headings = ["未決", "矛盾"]
pages    = [{ path = "log.md" }]
"#,
        None,
    )
    .unwrap();
    let r = check(&bundle, &cfg, Some(Path::new("okfast.toml"))).unwrap();
    let w = warnings(&r).join("\n");
    assert!(w.contains("キー credibility を持つページがない"), "{w}");
    assert!(w.contains("footnote_source: 日付の取れた行がない"), "{w}");
    assert!(w.contains("列「日にち」を持つ表がない"), "{w}");
    assert!(w.contains("path「log.md」に当たるページがない"), "{w}");
    assert!(!w.contains("table_row"), "{w}");
    let all = all_messages(&r);
    assert!(all.contains("table_row: 日付の取れた行 1"), "{all}");
    assert!(all.contains("見出しが当たる節: 1"), "{all}");
    let _ = std::fs::remove_dir_all(&root);
}
