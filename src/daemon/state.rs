//! 常駐プロセスが持つ 1 バンドルぶんの状態と、ファイルの監視。
//!
//! - 読み込んだページ・正規化済みの本文・辞書を、すべてメモリに持つ
//! - ファイルの変更は OS の通知（Windows は ReadDirectoryChangesW、Linux は inotify）で受け、
//!   変わったページだけを読み直す。通知があふれたとき・ディレクトリが動いたときは走査し直す
//! - 問い合わせの前に、届いている変更をすべて反映してから答える

use crate::aliases::Dictionary;
use crate::bundle::{Bundle, CacheMode, is_page_path};
use crate::config::{self, Config};
use crate::error::{Error, Result};
use notify::event::{AccessKind, AccessMode, ModifyKind};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use rayon::prelude::*;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, RwLock, Weak};
use std::time::{Duration, Instant, SystemTime};

/// ある時点のバンドル・設定・辞書。検索はこれを借りて行い、更新は丸ごと差し替える。
#[derive(Debug)]
pub struct Snapshot {
    /// バンドル。
    pub bundle: Bundle,
    /// 設定。
    pub config: Config,
    /// 設定ファイルのパス（なければ `None`）。
    pub config_path: Option<PathBuf>,
    /// 辞書。
    pub dict: Dictionary,
    /// 本文の合計バイト数。
    pub bytes: usize,
}

#[derive(Debug, Default)]
struct Pending {
    rels: BTreeSet<String>,
    rescan: bool,
    config: bool,
}

impl Pending {
    fn is_dirty(&self) -> bool {
        self.rescan || self.config || !self.rels.is_empty()
    }
}

type ConfigStamp = Option<(u64, Option<SystemTime>)>;

fn config_stamp(path: &Path) -> ConfigStamp {
    std::fs::metadata(path)
        .ok()
        .map(|m| (m.len(), m.modified().ok()))
}

/// 開けなかった理由。
#[derive(Debug)]
pub enum OpenError {
    /// 上限を超える。
    TooLarge(usize),
    /// 読み込み・設定のエラー。
    Failed(Error),
}

impl From<Error> for OpenError {
    fn from(e: Error) -> Self {
        OpenError::Failed(e)
    }
}

/// 更新の回数など。
#[derive(Debug, Default, Clone, Copy)]
pub struct Counters {
    /// 変更を反映した回数。
    pub updates: u64,
    /// 走査し直した回数。
    pub rescans: u64,
    /// 読み直したページの延べ数。
    pub reread: u64,
}

/// 監視しているバンドル。
#[derive(Debug)]
pub struct Watched {
    root: PathBuf,
    config_arg: Option<PathBuf>,
    config_file: PathBuf,
    max_bytes: usize,
    snap: RwLock<Arc<Snapshot>>,
    pending: Arc<(Mutex<Pending>, Condvar)>,
    /// 反映に失敗した変更。裏の反映では触らず、次の問い合わせでやり直す（失敗を繰り返し続けないため）。
    retry: Mutex<Pending>,
    apply: Mutex<ConfigStamp>,
    counters: Mutex<Counters>,
    last_used: Mutex<Instant>,
    _watcher: Mutex<RecommendedWatcher>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn classify(root: &Path, path: &Path, kind: EventKind, p: &mut Pending) {
    let Ok(rel) = path.strip_prefix(root) else {
        return;
    };
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    if parts.is_empty() {
        p.rescan = true;
        return;
    }
    if parts.iter().any(|c| c.starts_with('.')) {
        return;
    }
    let rel = parts.join("/");
    if is_page_path(&rel) {
        p.rels.insert(rel);
        return;
    }
    if rel == config::FILE_NAME {
        p.config = true;
        return;
    }
    // ディレクトリの追加・削除・名前の変更は、中のページに通知が来ないことがある
    // 中のファイルの変更に伴うディレクトリ自体の更新（Modify）は、ファイルの通知で足りる
    match std::fs::metadata(path) {
        Ok(m) if m.is_dir() && matches!(kind, EventKind::Modify(ModifyKind::Name(_))) => {
            p.rescan = true;
        }
        Ok(m) if m.is_dir() && !matches!(kind, EventKind::Modify(_)) => p.rescan = true,
        Ok(_) => {}
        Err(_) => {
            let name = parts.last().map_or("", String::as_str);
            if !name.contains('.') || name.ends_with(".md") {
                p.rescan = true;
            }
        }
    }
}

fn build_snapshot(
    bundle: Bundle,
    config: Config,
    config_path: Option<PathBuf>,
) -> Result<Snapshot> {
    bundle.pages.par_iter().for_each(|p| {
        p.text_index();
    });
    let dict = Dictionary::build(&bundle, &config.aliases)?;
    let bytes = bundle.total_bytes();
    Ok(Snapshot {
        bundle,
        config,
        config_path,
        dict,
        bytes,
    })
}

impl Watched {
    /// 読み込んで監視を始める。`max_bytes` を超えるバンドルは開かない。
    pub fn open(
        root: &Path,
        config_arg: Option<&Path>,
        max_bytes: usize,
    ) -> std::result::Result<Arc<Self>, OpenError> {
        // 読み込みの最中の変更を落とさないよう、監視を先に始める
        let pending: Arc<(Mutex<Pending>, Condvar)> = Arc::default();
        let sink = Arc::clone(&pending);
        let watch_root = root.to_path_buf();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
            let (m, cv) = &*sink;
            let mut p = lock(m);
            match res {
                Ok(ev) => {
                    if let EventKind::Access(a) = ev.kind {
                        if a != AccessKind::Close(AccessMode::Write) {
                            return;
                        }
                    }
                    if ev.need_rescan() {
                        p.rescan = true;
                    }
                    for path in &ev.paths {
                        classify(&watch_root, path, ev.kind, &mut p);
                    }
                }
                Err(_) => p.rescan = true,
            }
            if p.is_dirty() {
                cv.notify_one();
            }
        })
        .map_err(|e| watch_error(root, e))?;
        watcher
            .watch(root, RecursiveMode::Recursive)
            .map_err(|e| watch_error(root, e))?;

        let (bundle, _) = Bundle::load_with(root, &CacheMode::Default)?;
        let bytes = bundle.total_bytes();
        if bytes > max_bytes {
            return Err(OpenError::TooLarge(bytes));
        }
        let config_file = config_arg
            .map(Path::to_path_buf)
            .unwrap_or_else(|| root.join(config::FILE_NAME));
        let stamp = config_stamp(&config_file);
        let (config, config_path) = Config::discover(root, config_arg)?;
        let snap = build_snapshot(bundle, config, config_path)?;

        let w = Arc::new(Watched {
            root: root.to_path_buf(),
            config_arg: config_arg.map(Path::to_path_buf),
            config_file,
            max_bytes,
            snap: RwLock::new(Arc::new(snap)),
            pending,
            retry: Mutex::default(),
            apply: Mutex::new(stamp),
            counters: Mutex::default(),
            last_used: Mutex::new(Instant::now()),
            _watcher: Mutex::new(watcher),
        });
        spawn_applier(Arc::downgrade(&w));
        Ok(w)
    }

    /// 届いている変更をすべて反映した状態を返す。
    pub fn current(&self) -> std::result::Result<Arc<Snapshot>, OpenError> {
        *lock(&self.last_used) = Instant::now();
        self.refresh(true)?;
        Ok(Arc::clone(
            &self
                .snap
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        ))
    }

    /// 最後に使われた時刻。
    pub fn last_used(&self) -> Instant {
        *lock(&self.last_used)
    }

    /// 本文の合計バイト数。
    pub fn bytes(&self) -> usize {
        self.snap
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .bytes
    }

    /// 更新の回数など。
    pub fn counters(&self) -> Counters {
        *lock(&self.counters)
    }

    /// root。
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 届いている変更を反映する。変更がなければ何もしない。
    /// `with_retry` が真なら、前に失敗した変更もやり直す。
    fn refresh(&self, with_retry: bool) -> std::result::Result<(), OpenError> {
        let mut stamp = lock(&self.apply);
        let mut taken = {
            let mut p = lock(&self.pending.0);
            std::mem::take(&mut *p)
        };
        if with_retry {
            let r = std::mem::take(&mut *lock(&self.retry));
            taken.rescan |= r.rescan;
            taken.config |= r.config;
            taken.rels.extend(r.rels);
        } else if lock(&self.retry).is_dirty() {
            // 失敗した変更が残っている間は、順序を保つため問い合わせの時にまとめて反映する
            let mut r = lock(&self.retry);
            r.rescan |= taken.rescan;
            r.config |= taken.config;
            r.rels.extend(taken.rels);
            return Ok(());
        }
        let new_stamp = config_stamp(&self.config_file);
        let config_changed = new_stamp != *stamp || taken.config;
        if !taken.rescan && taken.rels.is_empty() && !config_changed {
            return Ok(());
        }
        match self.apply_changes(&taken) {
            Ok(reread) => {
                *stamp = new_stamp;
                let mut c = lock(&self.counters);
                c.updates += 1;
                c.rescans += u64::from(taken.rescan);
                c.reread += reread as u64;
                Ok(())
            }
            Err(e) => {
                // 反映できなかった変更は取っておき、次の問い合わせでやり直す
                let mut r = lock(&self.retry);
                r.rescan |= taken.rescan;
                r.config |= taken.config;
                r.rels.extend(taken.rels);
                Err(e)
            }
        }
    }

    fn apply_changes(&self, taken: &Pending) -> std::result::Result<usize, OpenError> {
        let old = Arc::clone(
            &self
                .snap
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        let (bundle, reread) = if taken.rescan {
            old.bundle.rescan()?
        } else {
            let rels: Vec<String> = taken.rels.iter().cloned().collect();
            (old.bundle.update(&rels)?, rels.len())
        };
        let bytes = bundle.total_bytes();
        if bytes > self.max_bytes {
            return Err(OpenError::TooLarge(bytes));
        }
        // 設定の読み直しは安いので、変更の有無にかかわらず読み直す
        let (config, config_path) = Config::discover(&self.root, self.config_arg.as_deref())?;
        let snap = build_snapshot(bundle, config, config_path)?;
        *self
            .snap
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::new(snap);
        Ok(reread)
    }
}

fn watch_error(root: &Path, e: notify::Error) -> OpenError {
    OpenError::Failed(Error::Io {
        path: root.to_path_buf(),
        source: std::io::Error::other(format!("ファイルの監視を始められない: {e}")),
    })
}

/// 変更が届いたら、問い合わせを待たずに反映しておく（問い合わせの時の待ちを減らす）。
fn spawn_applier(w: Weak<Watched>) {
    let Some(strong) = w.upgrade() else { return };
    let pending = Arc::clone(&strong.pending);
    drop(strong);
    std::thread::spawn(move || {
        loop {
            {
                let (m, cv) = &*pending;
                let mut p = lock(m);
                while !p.is_dirty() {
                    let (g, timeout) = cv
                        .wait_timeout(p, Duration::from_secs(5))
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    p = g;
                    if timeout.timed_out() && w.strong_count() == 0 {
                        return;
                    }
                }
            }
            // 続けて届く変更（保存の直後の書き込みなど）をまとめる
            std::thread::sleep(Duration::from_millis(15));
            let Some(watched) = w.upgrade() else { return };
            let _ = watched.refresh(false);
        }
    });
}
