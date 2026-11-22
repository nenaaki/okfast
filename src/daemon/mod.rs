//! 常駐プロセス。バンドルをすべてメモリに持ち、ファイルの変更を監視して答える。
//!
//! - `okfast grep` などは、まず常駐プロセスに問い合わせる。いなければその回は自分で実行し、
//!   常駐プロセスを裏で起動しておく（次の回から速くなる）
//! - 本文の合計が上限（既定 256 MiB、`OKFAST_DAEMON_MAX_MB`）を超えるバンドルは扱わず、呼んだ側で実行させる
//! - しばらく使われなければ終わる（既定 30 分、`OKFAST_DAEMON_IDLE_SECS`）
//! - 答えが上限（既定 30 秒、`OKFAST_DAEMON_TIMEOUT_SECS`）までに来なければ、呼んだ側で実行し、常駐プロセスを終わらせる
//! - `OKFAST_DAEMON=0` で使わない

pub mod ipc;
pub mod protocol;
pub mod state;

use crate::command::{self, Command, Context};
use protocol::{Request, RequestBody, Response};
use state::{OpenError, Watched};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 本文の合計の上限（既定）。
pub const DEFAULT_MAX_MB: usize = 256;
/// 使われなくなってから終わるまで（既定）。
pub const DEFAULT_IDLE_SECS: u64 = 30 * 60;
/// 応答を待つ上限（既定）。
pub const DEFAULT_TIMEOUT_SECS: u64 = 30;

fn env_num(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

/// 常駐プロセスを使うか（`OKFAST_DAEMON=0` なら使わない）。
pub fn enabled() -> bool {
    !matches!(
        std::env::var("OKFAST_DAEMON").as_deref().map(str::trim),
        Ok("0" | "off" | "false" | "no")
    )
}

/// この実行ファイルの識別子（版・サイズ・更新日時）。違えば常駐プロセスを入れ替える。
pub fn build_id() -> String {
    let meta = std::env::current_exe().and_then(std::fs::metadata);
    let (len, mtime) = match &meta {
        Ok(m) => (
            m.len(),
            m.modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos()),
        ),
        Err(_) => (0, 0),
    };
    format!("{}-{len:x}-{mtime:x}", env!("CARGO_PKG_VERSION"))
}

type Key = (PathBuf, Option<PathBuf>);

struct Server {
    build_id: String,
    max_bytes: usize,
    started: Instant,
    bundles: Mutex<HashMap<Key, Arc<Watched>>>,
    too_large: Mutex<HashMap<Key, (usize, Instant)>>,
    last_request: Mutex<Instant>,
    shutdown: Mutex<bool>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn mb(bytes: usize) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

impl Server {
    fn watched(&self, key: &Key) -> std::result::Result<Arc<Watched>, OpenError> {
        if let Some((n, at)) = lock(&self.too_large).get(key) {
            if at.elapsed() < Duration::from_secs(60) {
                return Err(OpenError::TooLarge(*n));
            }
        }
        let mut map = lock(&self.bundles);
        if let Some(w) = map.get(key) {
            return Ok(Arc::clone(w));
        }
        match Watched::open(&key.0, key.1.as_deref(), self.max_bytes) {
            Ok(w) => {
                // 合計が上限を超えるなら、使われていないものから手放す
                let mut total: usize = map.values().map(|w| w.bytes()).sum::<usize>() + w.bytes();
                while total > self.max_bytes {
                    let Some(k) = map
                        .iter()
                        .min_by_key(|(_, w)| w.last_used())
                        .map(|(k, _)| k.clone())
                    else {
                        break;
                    };
                    if let Some(old) = map.remove(&k) {
                        total -= old.bytes();
                    }
                }
                map.insert(key.clone(), Arc::clone(&w));
                Ok(w)
            }
            Err(OpenError::TooLarge(n)) => {
                lock(&self.too_large).insert(key.clone(), (n, Instant::now()));
                Err(OpenError::TooLarge(n))
            }
            Err(e) => Err(e),
        }
    }

    fn handle(&self, req: Request) -> Response {
        *lock(&self.last_request) = Instant::now();
        match req.body {
            RequestBody::Ping => Response::Info(self.build_id.clone()),
            RequestBody::Shutdown => {
                *lock(&self.shutdown) = true;
                Response::Info("終了する".into())
            }
            RequestBody::Status => Response::Info(self.status()),
            RequestBody::Run { .. } if req.build_id != self.build_id => {
                Response::WrongBuild(self.build_id.clone())
            }
            RequestBody::Run {
                root,
                config,
                stats,
                command,
            } => self.run(root, config, stats, &command),
        }
    }

    fn run(&self, root: PathBuf, config: Option<PathBuf>, stats: bool, cmd: &Command) -> Response {
        let started = Instant::now();
        let key = (root, config);
        let w = match self.watched(&key) {
            Ok(w) => w,
            Err(OpenError::TooLarge(n)) => {
                return Response::Fallback(format!(
                    "本文の合計 {:.1} MiB が常駐の上限 {:.0} MiB を超える",
                    mb(n),
                    mb(self.max_bytes)
                ));
            }
            Err(OpenError::Failed(e)) => return error_response(&e),
        };
        let snap = match w.current() {
            Ok(s) => s,
            Err(OpenError::TooLarge(n)) => {
                lock(&self.bundles).remove(&key);
                return Response::Fallback(format!(
                    "本文の合計 {:.1} MiB が常駐の上限を超えた",
                    mb(n)
                ));
            }
            Err(OpenError::Failed(e)) => return error_response(&e),
        };
        let cx = Context {
            bundle: &snap.bundle,
            dict: &snap.dict,
            config: &snap.config,
            has_config: snap.config_path.is_some(),
        };
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = match command::run(cmd, &cx, &mut stdout) {
            Ok(c) => c,
            Err(e) => return error_response(&e),
        };
        if stats {
            let c = w.counters();
            let _ = writeln!(
                stderr,
                "okfast: 常駐プロセスで実行 {:?}（ページ {}・本文 {:.1} MiB、更新の反映 {} 回・読み直し {} ページ・走査し直し {} 回）",
                started.elapsed(),
                snap.bundle.pages.len(),
                mb(snap.bytes),
                c.updates,
                c.reread,
                c.rescans
            );
        }
        Response::Done {
            code,
            stdout,
            stderr,
        }
    }

    fn status(&self) -> String {
        let mut s = format!(
            "常駐プロセス: {}（起動から {:?}、ビルド {}）\n上限 {:.0} MiB\n",
            ipc::endpoint(),
            Duration::from_secs(self.started.elapsed().as_secs()),
            self.build_id,
            mb(self.max_bytes)
        );
        for w in lock(&self.bundles).values() {
            let c = w.counters();
            s.push_str(&format!(
                "- {}: 本文 {:.1} MiB、最後の利用から {:?}、更新の反映 {} 回\n",
                w.root().display(),
                mb(w.bytes()),
                Duration::from_secs(w.last_used().elapsed().as_secs()),
                c.updates
            ));
        }
        s
    }
}

fn error_response(e: &crate::Error) -> Response {
    Response::Done {
        code: 2,
        stdout: Vec::new(),
        stderr: format!("okfast: {e}\n").into_bytes(),
    }
}

/// 常駐プロセスとして動く（戻らない）。ほかの常駐プロセスがいれば、すぐに終わる。
pub fn serve(build_id: String, preload: Option<Key>) -> std::io::Result<()> {
    let mut listener = match ipc::Listener::bind() {
        Ok(l) => l,
        Err(_) => return Ok(()),
    };
    let max_mb = env_num("OKFAST_DAEMON_MAX_MB", DEFAULT_MAX_MB as u64) as usize;
    let idle = Duration::from_secs(env_num("OKFAST_DAEMON_IDLE_SECS", DEFAULT_IDLE_SECS));
    let server = Arc::new(Server {
        build_id,
        max_bytes: max_mb.saturating_mul(1024 * 1024),
        started: Instant::now(),
        bundles: Mutex::default(),
        too_large: Mutex::default(),
        last_request: Mutex::new(Instant::now()),
        shutdown: Mutex::new(false),
    });

    if let Some(key) = preload {
        let s = Arc::clone(&server);
        std::thread::spawn(move || {
            let _ = s.watched(&key);
        });
    }

    let guard = listener.guard();
    {
        let s = Arc::clone(&server);
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(2));
                let idle_for = lock(&s.last_request).elapsed();
                if idle_for > idle || !guard.is_current() || *lock(&s.shutdown) {
                    guard.cleanup();
                    std::process::exit(0);
                }
            }
        });
    }

    loop {
        let conn = match listener.accept() {
            Ok(c) => c,
            Err(_) => continue,
        };
        let s = Arc::clone(&server);
        let guard = listener.guard();
        std::thread::spawn(move || {
            let mut conn = conn;
            if let Ok(req) = Request::read(&mut conn) {
                let shutdown = matches!(req.body, RequestBody::Shutdown);
                let res = s.handle(req);
                let _ = res.write(&mut conn);
                ipc::finish(conn);
                if shutdown {
                    guard.cleanup();
                    std::process::exit(0);
                }
            }
        });
    }
}

/// 呼んだ側の結果。
#[derive(Debug)]
pub enum Outcome {
    /// 常駐プロセスが答えた。
    Done {
        /// 終了コード。
        code: u8,
        /// 標準出力。
        stdout: Vec<u8>,
        /// 標準エラー。
        stderr: Vec<u8>,
    },
    /// 使えなかった（理由）。呼んだ側で実行する。
    Unavailable(String),
}

fn request(body: RequestBody) -> std::io::Result<Response> {
    let mut conn = ipc::connect()?;
    Request {
        build_id: build_id(),
        body,
    }
    .write(&mut conn)?;
    Response::read(&mut conn)
}

/// 応答を待つ上限つきで頼む。常駐プロセスが固まっても、呼んだ側を止めないため
/// （Windows の名前付きパイプには読み込みの時間切れがない）。
fn request_within(body: RequestBody, timeout: Duration) -> std::io::Result<Response> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(request(body));
    });
    match rx.recv_timeout(timeout) {
        Ok(r) => r,
        Err(_) => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!("常駐プロセスが {} 秒以内に答えない", timeout.as_secs_f32()),
        )),
    }
}

/// 応答を待つ上限（`OKFAST_DAEMON_TIMEOUT_SECS`、既定 30 秒）。大きなバンドルの最初の読み込みも収まる長さ。
fn timeout() -> Duration {
    Duration::from_secs(env_num("OKFAST_DAEMON_TIMEOUT_SECS", DEFAULT_TIMEOUT_SECS))
}

/// 常駐プロセスにコマンドを頼む。いなければ裏で起動し、`Unavailable` を返す。
pub fn run(root: &Path, config: Option<&Path>, stats: bool, cmd: &Command) -> Outcome {
    let root = match std::fs::canonicalize(root) {
        Ok(r) => r,
        Err(e) => return Outcome::Unavailable(format!("root を解決できない: {e}")),
    };
    let config = match config.map(std::path::absolute).transpose() {
        Ok(c) => c,
        Err(e) => return Outcome::Unavailable(format!("設定のパスを解決できない: {e}")),
    };
    let body = RequestBody::Run {
        root: root.clone(),
        config: config.clone(),
        stats,
        command: cmd.clone(),
    };
    match request_within(body, timeout()) {
        Ok(Response::Done {
            code,
            stdout,
            stderr,
        }) => Outcome::Done {
            code,
            stdout,
            stderr,
        },
        Ok(Response::Fallback(why)) => Outcome::Unavailable(why),
        Ok(Response::WrongBuild(other)) => {
            let _ = request_within(RequestBody::Shutdown, Duration::from_secs(1));
            // 古い常駐プロセスが名前を手放すのを少し待ってから起動する
            std::thread::sleep(Duration::from_millis(50));
            let _ = spawn(&root, config.as_deref());
            Outcome::Unavailable(format!(
                "常駐プロセスのビルドが違うため入れ替える（{other}）"
            ))
        }
        Ok(Response::Info(_)) => Outcome::Unavailable("想定外の応答".into()),
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
            // 固まった常駐プロセスに終わるよう頼む（これも待ちすぎない）。次の呼び出しで起動し直す
            let _ = request_within(RequestBody::Shutdown, Duration::from_secs(1));
            Outcome::Unavailable(e.to_string())
        }
        Err(e) => {
            let started = spawn(&root, config.as_deref());
            Outcome::Unavailable(match started {
                Ok(()) => format!("常駐プロセスがいないため起動した（{e}）"),
                Err(se) => format!("常駐プロセスを起動できない: {se}"),
            })
        }
    }
}

/// 常駐プロセスの状態を聞く。
pub fn status() -> std::io::Result<String> {
    match request_within(RequestBody::Status, timeout())? {
        Response::Info(s) => Ok(s),
        other => Ok(format!("{other:?}")),
    }
}

/// 常駐プロセスを終わらせる。
pub fn stop() -> std::io::Result<()> {
    request_within(RequestBody::Shutdown, timeout()).map(|_| ())
}

/// 常駐プロセスを裏で起動する（待たない）。
pub fn spawn(root: &Path, config: Option<&Path>) -> std::io::Result<()> {
    let id = build_id();
    let exe = daemon_exe(&id)?;
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("daemon")
        .arg("serve")
        .arg("--build-id")
        .arg(&id)
        .arg("--bundle")
        .arg(root);
    if let Some(c) = config {
        cmd.arg("--config").arg(c);
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .current_dir(std::env::temp_dir());
    detach(&mut cmd)
}

#[cfg(windows)]
fn detach(cmd: &mut std::process::Command) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, SetHandleInformation};
    use windows_sys::Win32::System::Console::{
        GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };
    // 自分の標準入出力を常駐プロセスに継がせない。継がせると、呼んだ側（パイプで出力を読む側）が
    // 常駐プロセスの終わりまで出力の終わりを待ち続ける
    for k in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: 自プロセスの標準ハンドルの継承の印を外すだけ。無効なハンドルなら失敗して何も起きない。
        unsafe {
            let h = GetStdHandle(k);
            if !h.is_null() {
                SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
    // 呼んだ側のジョブ（終了時に子を道連れにするもの）から抜ける。許されなければ抜けずに起動する
    let base = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
    match cmd.creation_flags(base | CREATE_BREAKAWAY_FROM_JOB).spawn() {
        Ok(_) => Ok(()),
        Err(_) => cmd.creation_flags(base).spawn().map(|_| ()),
    }
}

#[cfg(unix)]
fn detach(cmd: &mut std::process::Command) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0).spawn().map(|_| ())
}

/// 常駐させる実行ファイル。Windows では実行中のファイルを上書きできない（ビルドが失敗する）ため、
/// 複製を置いてそれを動かす。
#[cfg(windows)]
fn daemon_exe(id: &str) -> std::io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let Some(dir) = crate::cache::default_dir() else {
        return Ok(exe);
    };
    let dir = dir.join("daemon");
    std::fs::create_dir_all(&dir)?;
    let target = dir.join(format!("okfast-{id}.exe"));
    if !target.is_file() {
        // 古い複製を片付ける（動いているものは消せないので、失敗は無視）
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let _ = std::fs::remove_file(e.path());
            }
        }
        let tmp = dir.join(format!("okfast-{id}.{}.tmp", std::process::id()));
        std::fs::copy(&exe, &tmp)?;
        if std::fs::rename(&tmp, &target).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }
    Ok(target)
}

#[cfg(unix)]
fn daemon_exe(_id: &str) -> std::io::Result<PathBuf> {
    std::env::current_exe()
}
