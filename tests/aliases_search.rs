//! 同義語の取り出しと検索の結合テスト（tests/fixtures/bundle）。

use okfast::aliases::Dictionary;
use okfast::bundle::Bundle;
use okfast::config::Config;
use okfast::search::{Options, Query, SearchResult, search};
use std::path::{Path, PathBuf};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bundle")
}

fn setup() -> (Bundle, Config, Dictionary) {
    let root = fixture();
    let (cfg, path) = Config::discover(&root, None).unwrap();
    assert!(path.is_some());
    let bundle = Bundle::load(&root).unwrap();
    let dict = Dictionary::build(&bundle, &cfg.aliases).unwrap();
    (bundle, cfg, dict)
}

fn run(words: &[&str], aliases: bool, all: bool) -> (Query, SearchResult<'static>) {
    let (bundle, cfg, dict) = setup();
    let bundle: &'static Bundle = Box::leak(Box::new(bundle));
    let words: Vec<String> = words.iter().map(|s| s.to_string()).collect();
    let q = Query::parse(&words, aliases.then_some(&dict)).unwrap();
    let opt = Options {
        exclude: cfg.search.exclude.clone(),
        include_all: all,
        short_max: cfg.aliases.matching.short_max,
        ..Default::default()
    };
    let r = search(bundle, &dict, &q, &opt).unwrap();
    (q, r)
}

fn hit_files<'a>(r: &SearchResult<'a>) -> Vec<&'a str> {
    r.files.iter().map(|f| f.rel).collect()
}

#[test]
fn loads_bundle_and_skips_reserved() {
    let (bundle, _, _) = setup();
    assert!(bundle.page("index.md").is_none());
    let other = bundle.page("notes/other.md").unwrap();
    // 未知の type・未知のキーもそのまま保持する
    assert_eq!(
        other.frontmatter().okf_type().as_deref(),
        Some("Some Unknown Type")
    );
    assert!(
        bundle
            .page("notes/widget.md")
            .unwrap()
            .frontmatter()
            .get("custom_key")
            .is_some()
    );
}

#[test]
fn table_source_with_trust_filter() {
    let (_, _, dict) = setup();
    // credibility: D の表は使わない
    let e = dict.expand(&["WP".to_string()]);
    assert_eq!(e.added, vec!["Widget Pro", "ウィジェットプロ"]);
    assert!(!e.added.iter().any(|n| n == "偽の別名"));
    assert_eq!(e.boost, vec!["notes/widget.md"]);
    assert_eq!(e.origins[0].page, "glossary/names.md");
    assert_eq!(e.origins[0].line, 10);
    // 概念名でも引ける。脚注は除かれる
    let e = dict.expand(&["widget pro".to_string()]);
    assert!(e.added.contains(&"ウィジェットプロ".to_string()));
}

#[test]
fn frontmatter_source() {
    let (_, _, dict) = setup();
    let e = dict.expand(&["例のアレ".to_string()]);
    assert_eq!(e.added, vec!["Thing", "Thingamajig"]);
    assert_eq!(e.boost, vec!["notes/other.md"]);
}

#[test]
fn no_transitive_expansion() {
    let (_, _, dict) = setup();
    let e = dict.expand(&["Gadget".to_string()]);
    assert_eq!(e.added, vec!["ガジェット"]);
}

#[test]
fn search_expands_and_boosts() {
    let (q, r) = run(&["WP"], true, false);
    assert!(!q.terms[0].expansion.is_empty());
    // 主題のページが先。文字起こしは誤認識（条件付きの呼び名）で当たる
    assert_eq!(
        hit_files(&r),
        vec![
            "notes/widget.md",
            "notes/other.md",
            "references/transcripts/rec1.md"
        ]
    );
    let other = &r.files[1];
    // 「WP」は単語として当たり、全角の「ｗｉｄｇｅｔ　ｐｒｏ」も正規化で当たる（行 7・8）
    let lines: Vec<usize> = other.hits.iter().map(|h| h.line).collect();
    assert_eq!(lines, vec![7, 8]);
    // 除外したページ（glossary/**）は出ない
    assert_eq!(r.excluded, 2);
}

#[test]
fn short_token_needs_word_boundary() {
    // 「WPA は別物。」は WP 単独では当たらない
    let (_, r) = run(&["WP"], false, false);
    let other = &r.files[0];
    assert!(other.hits.iter().all(|h| !h.text.starts_with("WPA")));
    let (_, r) = run(&["WPA"], false, false);
    assert_eq!(r.total_lines(), 1);
}

#[test]
fn trust_requires_the_key() {
    // trust を設定したとき、キーを持たないページは取り出し元にしない
    let (_, _, dict) = setup();
    assert!(!dict.source_pages.iter().any(|p| p == "notes/widget.md"));
    assert!(dict.source_pages.iter().any(|p| p == "notes/other.md"));
}

#[test]
fn search_without_aliases() {
    let (_, r) = run(&["WP"], false, false);
    assert_eq!(hit_files(&r), vec!["notes/other.md"]);
}

#[test]
fn conditional_names_apply_only_in_scope() {
    // 文字起こしのページでは誤認識で当たる
    let (_, r) = run(&["Widget Pro"], true, false);
    assert!(hit_files(&r).contains(&"references/transcripts/rec1.md"));
    // 誤認識の語を直接探しても、言い換えは文字起こしの外へ広げない
    let (_, r) = run(&["ウジェットプロ"], true, false);
    assert_eq!(hit_files(&r), vec!["references/transcripts/rec1.md"]);
}

#[test]
fn include_all_searches_excluded() {
    let (_, r) = run(&["偽の別名"], true, true);
    assert_eq!(hit_files(&r), vec!["glossary/untrusted.md"]);
}

#[test]
fn user_alternatives_and_multiple_terms() {
    let (q, r) = run(&["Thingamajig|存在しない", "Gadget"], false, false);
    assert_eq!(q.terms[0].alts.len(), 2);
    assert_eq!(r.lines_per_term(2), vec![0, 1]);
}
