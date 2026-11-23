//! 処理の段階ごとの時間を測る。
//! cargo run --release --example timing -- <bundle> <config> <語>...

use okfast::aliases::Dictionary;
use okfast::bundle::{Bundle, CacheMode};
use okfast::command::SearchOptions;
use okfast::config::Config;
use okfast::ranges::{RangeOptions, search_ranges};
use okfast::search::{Options, Query, search};
use std::path::Path;
use std::time::{Duration, Instant};

fn best<T>(n: usize, mut f: impl FnMut() -> T) -> (Duration, T) {
    let mut best = Duration::MAX;
    let mut out = None;
    for _ in 0..n {
        let t = Instant::now();
        let v = f();
        best = best.min(t.elapsed());
        out = Some(v);
    }
    (best, out.unwrap())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = Path::new(&args[0]);
    let cfg = Config::load(Path::new(&args[1])).unwrap();
    let cache_dir = std::env::temp_dir().join("okfast-timing-cache");
    let _ = std::fs::remove_dir_all(&cache_dir);
    let mode = CacheMode::Dir(cache_dir.clone());

    let (no_cache, _) = best(3, || Bundle::load_with(root, &CacheMode::Off).unwrap());
    let (cold, _) = best(1, || Bundle::load_with(root, &mode).unwrap());
    let (warm, (bundle, stats)) = best(5, || Bundle::load_with(root, &mode).unwrap());
    let bytes: usize = bundle.pages.iter().map(|p| p.text().len()).sum();

    let (dict_t, dict) = best(5, || Dictionary::build(&bundle, &cfg.aliases).unwrap());
    let words: Vec<String> = args[2..].to_vec();
    let q = Query::parse(&words, Some(&dict)).unwrap();
    let opt = Options {
        exclude: cfg.search.exclude.clone(),
        include_all: false,
        short_max: cfg.aliases.matching.short_max,
        ..Default::default()
    };
    let (first_t, _) = best(1, || search(&bundle, &dict, &q, &opt).unwrap());
    let (search_t, r) = best(5, || search(&bundle, &dict, &q, &opt).unwrap());
    let ropt = RangeOptions {
        short_max: cfg.aliases.matching.short_max,
        top: SearchOptions::TOP,
        per_page: SearchOptions::PER_PAGE,
        recent: cfg.search.recent.count,
        ..Default::default()
    };
    let (ranges_t, rr) = best(5, || {
        search_ranges(&bundle, &dict, &q, &cfg.search, &ropt).unwrap()
    });
    let index_bytes: usize = bundle
        .pages
        .iter()
        .map(|p| p.text_index().heap_bytes())
        .sum();

    println!(
        "pages {} / {:.1} MB",
        bundle.pages.len(),
        bytes as f64 / 1e6
    );
    println!("load, no cache            {no_cache:?}");
    println!("load, cache cold (write)  {cold:?}");
    println!(
        "load, cache warm          {warm:?}  (cached {}, read {})",
        stats.cached, stats.read
    );
    println!("Dictionary::build         {dict_t:?}");
    println!("search, first (builds index) {first_t:?}");
    println!(
        "search, in memory ({} lines) {search_t:?}  (index {:.1} MB)",
        r.total_lines(),
        index_bytes as f64 / 1e6
    );
    println!("search_ranges ({} ranges)  {ranges_t:?}", rr.total_ranges());
    let _ = std::fs::remove_dir_all(&cache_dir);
}
