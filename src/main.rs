use clap::{Parser, Subcommand};
use okfast::aliases::Dictionary;
use okfast::bundle::{Bundle, CacheMode};
use okfast::check::{self, Level};
use okfast::config::Config;
use okfast::{command, daemon};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::process::ExitCode;

/// OKF 0.2 バンドルを高速に扱うツール。
#[derive(Parser, Debug)]
#[command(version, about)]
struct Cli {
    /// バンドルの root。
    #[arg(short, long, global = true, default_value = ".")]
    bundle: PathBuf,
    /// 設定ファイル（省略時は <bundle>/okfast.toml）。
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// 読み込んだページのキャッシュを使わない。
    #[arg(long, global = true)]
    no_cache: bool,
    /// キャッシュの置き場所（省略時は OKFAST_CACHE_DIR か %LOCALAPPDATA%\okfast）。
    #[arg(long, global = true, conflicts_with = "no_cache")]
    cache_dir: Option<PathBuf>,
    /// 読み込みの内訳（キャッシュの当たり・時間）を標準エラーに出す。
    #[arg(long, global = true)]
    stats: bool,
    /// 常駐プロセスを使わない（環境変数 OKFAST_DAEMON=0 でも同じ）。
    #[arg(long, global = true)]
    no_daemon: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// 語を検索する。語の中の `|` は言い換え。同義語の辞書で自動的に言い換える。
    Grep {
        /// 検索語（複数指定はどれかに当たる行）。
        #[arg(required = true)]
        terms: Vec<String>,
        /// 辞書で言い換えない。
        #[arg(long)]
        no_aliases: bool,
        /// 設定の exclude を無視してすべてのページを検索する。
        #[arg(long)]
        all: bool,
        /// ページごとの行数だけを出す。
        #[arg(short, long, conflicts_with = "files")]
        count: bool,
        /// 当たったページのパスだけを出す（順位順）。
        #[arg(short = 'l', long)]
        files: bool,
        /// 1 ページに出す当たった行の数（0 は無制限）。
        #[arg(short = 'm', long, default_value_t = command::GrepOptions::MAX_PER_PAGE)]
        max_per_page: usize,
        /// 行を出すページの数（0 は無制限）。残りはパスと行数の一覧になる。
        #[arg(long, default_value_t = command::GrepOptions::MAX_PAGES)]
        max_pages: usize,
        /// 当たった行の前後に出す行の数。
        #[arg(short = 'C', long, default_value_t = 0)]
        context: usize,
        /// 1 行に出す文字数（0 は無制限）。
        #[arg(long, default_value_t = command::GrepOptions::MAX_COLUMNS)]
        max_columns: usize,
        /// 上限なしですべて出す（-m 0 --max-pages 0 --max-columns 0 と同じ）。
        #[arg(long)]
        full: bool,
    },
    /// 語がどう言い換えられるかを表示する。
    Aliases {
        /// 語。
        #[arg(required = true)]
        terms: Vec<String>,
        /// JSON で出す（語ごとに 1 行）。
        #[arg(long)]
        json: bool,
    },
    /// 設定（okfast.toml）を確かめる・下書きを作る・書式を見る。
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// 常駐プロセス（すべてをメモリに持ち、ファイルの変更を監視して答える）を操作する。
    /// grep と aliases は自動で常駐プロセスを使う。
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },
}

#[derive(Subcommand, Debug)]
enum DaemonAction {
    /// 状態を表示する。
    Status,
    /// 終わらせる。
    Stop,
    /// 裏で起動する（-b のバンドルを先に読み込む）。
    Start,
    /// 常駐プロセスとして動く（内部用）。
    #[command(hide = true)]
    Serve {
        /// 起動した側のビルドの識別子。
        #[arg(long)]
        build_id: String,
    },
}

#[derive(Subcommand, Debug)]
enum ConfigAction {
    /// 設定をバンドルに当てて確かめる。終了コードは警告なしが 0、警告ありが 1、設定のエラーが 2。
    Check,
    /// バンドルを走査して、設定の下書きを標準出力に出す（ファイルは書かない）。
    Init,
    /// 設定の書式の説明を出す。
    Reference,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("okfast: {e}");
            ExitCode::from(2)
        }
    }
}

fn load_bundle(cli: &Cli) -> okfast::Result<Bundle> {
    let started = std::time::Instant::now();
    let mode = match (&cli.cache_dir, cli.no_cache) {
        (_, true) => CacheMode::Off,
        (Some(d), _) => CacheMode::Dir(d.clone()),
        (None, false) => CacheMode::Default,
    };
    let (bundle, load) = Bundle::load_with(&cli.bundle, &mode)?;
    if let Some(e) = &load.cache_error {
        eprintln!("okfast: キャッシュを書けなかった（検索は続ける）: {e}");
    }
    if cli.stats {
        eprintln!(
            "okfast: 読み込み {:?}（ページ {}: キャッシュ {}・読み込み {}・削除 {}、キャッシュ{}: {}）",
            started.elapsed(),
            bundle.pages.len(),
            load.cached,
            load.read,
            load.removed,
            if load.cache_written { "を更新" } else { "" },
            load.cache_file
                .as_deref()
                .map_or("なし".to_string(), |p| p.display().to_string())
        );
    }
    Ok(bundle)
}

fn run_config(cli: &Cli, action: &ConfigAction) -> okfast::Result<ExitCode> {
    let stdout = std::io::stdout();
    let mut out = BufWriter::new(stdout.lock());
    match action {
        ConfigAction::Reference => {
            let _ = out.write_all(okfast::config::REFERENCE.as_bytes());
            Ok(ExitCode::SUCCESS)
        }
        ConfigAction::Init => {
            let bundle = load_bundle(cli)?;
            let _ = out.write_all(okfast::init::draft(&bundle).as_bytes());
            Ok(ExitCode::SUCCESS)
        }
        ConfigAction::Check => {
            let (config, config_path) = Config::discover(&cli.bundle, cli.config.as_deref())?;
            let bundle = load_bundle(cli)?;
            let report = check::check(&bundle, &config, config_path.as_deref())?;
            for s in &report.sections {
                let _ = writeln!(out, "== {}", s.title);
                for f in &s.findings {
                    let mark = match f.level {
                        Level::Warn => "!",
                        Level::Content => "?",
                        Level::Info => "-",
                    };
                    let _ = writeln!(out, "  {mark} {}", f.message);
                }
            }
            let n = report.warnings();
            let c = report.content_notes();
            let _ = writeln!(out);
            if c > 0 {
                let _ = writeln!(
                    out,
                    "辞書の中身について {c} 件（? の行）。意図どおりかを利用者に確かめる（終了コードには影響しない）"
                );
            }
            if n == 0 {
                let _ = writeln!(out, "設定の警告なし");
                Ok(ExitCode::SUCCESS)
            } else {
                let _ = writeln!(
                    out,
                    "設定の警告 {n} 件（! の行）。書式は okfast config reference"
                );
                Ok(ExitCode::from(1))
            }
        }
    }
}

fn run(cli: Cli) -> okfast::Result<ExitCode> {
    let cmd = match &cli.command {
        Command::Config { action } => return run_config(&cli, action),
        Command::Daemon { action } => return run_daemon(&cli, action),
        Command::Aliases { terms, json } => command::Command::Aliases {
            terms: terms.clone(),
            json: *json,
        },
        Command::Grep {
            terms,
            no_aliases,
            all,
            count,
            files,
            max_per_page,
            max_pages,
            context,
            max_columns,
            full,
        } => command::Command::Grep {
            terms: terms.clone(),
            opts: command::GrepOptions {
                no_aliases: *no_aliases,
                all: *all,
                count: *count,
                files: *files,
                max_per_page: if *full { 0 } else { *max_per_page },
                max_pages: if *full { 0 } else { *max_pages },
                context: *context,
                max_columns: if *full { 0 } else { *max_columns },
            },
        },
    };

    if !cli.no_daemon && daemon::enabled() {
        let started = std::time::Instant::now();
        match daemon::run(&cli.bundle, cli.config.as_deref(), cli.stats, &cmd) {
            daemon::Outcome::Done {
                code,
                stdout,
                stderr,
            } => {
                let _ = std::io::stdout().lock().write_all(&stdout);
                let _ = std::io::stderr().lock().write_all(&stderr);
                if cli.stats {
                    eprintln!("okfast: 問い合わせ全体 {:?}", started.elapsed());
                }
                return Ok(ExitCode::from(code));
            }
            daemon::Outcome::Unavailable(why) => {
                if cli.stats {
                    eprintln!("okfast: 常駐プロセスを使わずに実行: {why}");
                }
            }
        }
    }

    let (config, config_path) = Config::discover(&cli.bundle, cli.config.as_deref())?;
    let bundle = load_bundle(&cli)?;
    let dict = Dictionary::build(&bundle, &config.aliases)?;
    let cx = command::Context {
        bundle: &bundle,
        dict: &dict,
        config: &config,
        has_config: config_path.is_some(),
    };
    let stdout = std::io::stdout();
    let mut out = BufWriter::new(stdout.lock());
    let code = command::run(&cmd, &cx, &mut out)?;
    let _ = out.flush();
    Ok(ExitCode::from(code))
}

fn run_daemon(cli: &Cli, action: &DaemonAction) -> okfast::Result<ExitCode> {
    let io = |e: std::io::Error| okfast::Error::Io {
        path: PathBuf::from(daemon::ipc::endpoint()),
        source: e,
    };
    match action {
        DaemonAction::Serve { build_id } => {
            let root = std::fs::canonicalize(&cli.bundle).ok();
            let config = cli
                .config
                .as_deref()
                .map(std::path::absolute)
                .transpose()
                .ok()
                .flatten();
            daemon::serve(build_id.clone(), root.map(|r| (r, config))).map_err(io)?;
            Ok(ExitCode::SUCCESS)
        }
        DaemonAction::Status => match daemon::status() {
            Ok(s) => {
                print!("{s}");
                Ok(ExitCode::SUCCESS)
            }
            Err(_) => {
                println!("常駐プロセスは動いていない（{}）", daemon::ipc::endpoint());
                Ok(ExitCode::from(1))
            }
        },
        DaemonAction::Stop => match daemon::stop() {
            Ok(()) => {
                println!("常駐プロセスを終わらせた");
                Ok(ExitCode::SUCCESS)
            }
            Err(_) => {
                println!("常駐プロセスは動いていない");
                Ok(ExitCode::SUCCESS)
            }
        },
        DaemonAction::Start => {
            daemon::spawn(&cli.bundle, cli.config.as_deref()).map_err(io)?;
            Ok(ExitCode::SUCCESS)
        }
    }
}
