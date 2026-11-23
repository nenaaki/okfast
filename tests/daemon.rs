//! 常駐プロセス: 自分で実行したときと同じ結果を返すこと、ファイルの変更をすぐに反映すること。

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

struct Env {
    root: PathBuf,
    work: PathBuf,
    endpoint: String,
}

impl Env {
    fn new(name: &str) -> Self {
        let work = std::env::temp_dir().join(format!("okfast-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&work);
        let root = work.join("bundle");
        copy_dir(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bundle"),
            &root,
        );
        let endpoint = if cfg!(windows) {
            format!(r"\\.\pipe\okfast-test-{name}-{}", std::process::id())
        } else {
            work.join("run")
                .join("d.sock")
                .to_string_lossy()
                .into_owned()
        };
        Env {
            root,
            work,
            endpoint,
        }
    }

    fn okfast(&self, args: &[&str]) -> Output {
        self.okfast_with(args, &[])
    }

    fn okfast_with(&self, args: &[&str], envs: &[(&str, &str)]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_okfast"))
            .arg("-b")
            .arg(&self.root)
            .args(args)
            .env("OKFAST_DAEMON_ENDPOINT", &self.endpoint)
            .env("OKFAST_CACHE_DIR", self.work.join("cache"))
            .env("OKFAST_DAEMON_IDLE_SECS", "60")
            .env_remove("OKFAST_DAEMON")
            .env_remove("OKFAST_DAEMON_TIMEOUT_SECS")
            .envs(envs.iter().copied())
            .output()
            .unwrap()
    }

    fn wait_ready(&self) {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(20) {
            if self.okfast(&["daemon", "status"]).status.success() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("常駐プロセスが起動しない");
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = self.okfast(&["daemon", "stop"]);
        std::thread::sleep(Duration::from_millis(100));
        let _ = std::fs::remove_dir_all(&self.work);
    }
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let p = e.path();
        if p.is_dir() {
            copy_dir(&p, &to.join(e.file_name()));
        } else {
            std::fs::copy(&p, to.join(e.file_name())).unwrap();
        }
    }
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn daemon_matches_local_and_sees_changes() {
    let env = Env::new("daemon");
    let local = env.okfast(&["--no-daemon", "grep", "Widget Pro", "SPK"]);
    assert!(local.status.success());

    // 1 回目: 常駐プロセスがいないので自分で実行し、裏で起動する
    let first = env.okfast(&["--stats", "grep", "Widget Pro", "SPK"]);
    assert_eq!(stdout(&first), stdout(&local));
    env.wait_ready();

    // 常駐プロセスが答え、結果は自分で実行したときと同じ
    let via = env.okfast(&["--stats", "grep", "Widget Pro", "SPK"]);
    let err = String::from_utf8_lossy(&via.stderr);
    assert!(err.contains("常駐プロセスで実行"), "{err}");
    assert_eq!(stdout(&via), stdout(&local));
    assert_eq!(via.status.code(), local.status.code());
    for args in [
        &["aliases", "WP"][..],
        &["aliases", "--json", "ウィジェットプロ"],
        &["grep", "-c", "ウィジェットプロ"],
        &["grep", "--all", "-l", "Widget"],
        &["grep", "存在しない語"],
        &["grep", "-C", "1", "-m", "1", "--max-pages", "1", "WP"],
        &["grep", "--full", "--max-columns", "5", "Widget"],
        &["grep", "--pages", "--all", "Widget"],
    ] {
        let mut no = vec!["--no-daemon"];
        no.extend_from_slice(args);
        let a = env.okfast(args);
        let b = env.okfast(&no);
        assert_eq!(stdout(&a), stdout(&b), "{args:?}");
        assert_eq!(a.status.code(), b.status.code(), "{args:?}");
    }

    // 追加・変更・削除を、書いた直後の問い合わせで反映する
    let page = env.root.join("notes").join("new.md");
    std::fs::write(&page, "---\ntype: Note\n---\nzqx first\n").unwrap();
    let o = env.okfast(&["grep", "-l", "zqx"]);
    assert!(stdout(&o).contains("notes/new.md"), "{}", stdout(&o));
    std::fs::write(&page, "---\ntype: Note\n---\nzqy second\n").unwrap();
    assert_eq!(env.okfast(&["grep", "zqx"]).status.code(), Some(1));
    assert!(env.okfast(&["grep", "zqy"]).status.success());
    std::fs::remove_file(&page).unwrap();
    assert_eq!(env.okfast(&["grep", "zqy"]).status.code(), Some(1));

    // ディレクトリの追加と名前の変更
    let dir = env.root.join("newdir");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("a.md"), "---\ntype: Note\n---\nzqw\n").unwrap();
    std::fs::rename(&dir, env.root.join("moved")).unwrap();
    let o = env.okfast(&["grep", "-l", "zqw"]);
    assert!(stdout(&o).contains("moved/a.md"), "{}", stdout(&o));
    assert!(!stdout(&o).contains("newdir/"), "{}", stdout(&o));

    // 答えが上限までに来なければ、自分で実行して同じ結果を返し、常駐プロセスを終わらせる
    let local = env.okfast(&["--no-daemon", "grep", "Widget"]);
    let timed = env.okfast_with(
        &["--stats", "grep", "Widget"],
        &[("OKFAST_DAEMON_TIMEOUT_SECS", "0")],
    );
    assert_eq!(stdout(&timed), stdout(&local));
    let err = String::from_utf8_lossy(&timed.stderr);
    assert!(err.contains("以内に答えない"), "{err}");
    let start = Instant::now();
    while env.okfast(&["daemon", "status"]).status.success() {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "常駐プロセスが終わらない"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    // 次の呼び出しで起動し直し、また常駐プロセスが答える
    let _ = env.okfast(&["grep", "Widget"]);
    env.wait_ready();

    // 設定の変更（辞書が変わる）
    let before = stdout(&env.okfast(&["aliases", "WP"]));
    std::fs::write(env.root.join("okfast.toml"), "").unwrap();
    let after = env.okfast(&["aliases", "WP"]);
    assert_ne!(stdout(&after), before);
    assert_eq!(
        stdout(&after),
        stdout(&env.okfast(&["--no-daemon", "aliases", "WP"]))
    );

    // 設定の誤りは、自分で実行したときと同じく終了コード 2
    std::fs::write(env.root.join("okfast.toml"), "[nope]\n").unwrap();
    assert_eq!(env.okfast(&["grep", "Widget"]).status.code(), Some(2));
}
