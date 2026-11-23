//! 常駐プロセスへの接続口。ポートは開けない。
//!
//! - Windows: 名前付きパイプ。所有者（と SYSTEM）だけが接続でき、リモートからの接続は拒む
//! - Unix: ユーザーだけが入れるディレクトリ（0700）の中の Unix ソケット（0600）

#[cfg(unix)]
pub use unix::*;
#[cfg(windows)]
pub use windows::*;

fn override_endpoint() -> Option<String> {
    std::env::var("OKFAST_DAEMON_ENDPOINT")
        .ok()
        .filter(|v| !v.trim().is_empty())
}

/// 名前の衝突を避けるための、ユーザーごとの短い識別子。
#[cfg(windows)]
fn user_tag() -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for k in ["USERDOMAIN", "USERNAME", "LOCALAPPDATA"] {
        for b in std::env::var(k).unwrap_or_default().bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x100_0000_01b3);
        }
    }
    format!("{h:016x}")
}

#[cfg(windows)]
mod windows {
    use std::fs::File;
    use std::io;
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use std::ptr::null_mut;
    use windows_sys::Win32::Foundation::{
        ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
        LocalFree,
    };
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_FIRST_PIPE_INSTANCE, FlushFileBuffers, PIPE_ACCESS_DUPLEX,
    };
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
        PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
        WaitNamedPipeW,
    };

    /// 接続。
    pub type Conn = File;

    /// 接続口の名前（`OKFAST_DAEMON_ENDPOINT` で変えられる。試験用）。やりとりの形式の版を含めるので、
    /// 形式の違う okfast は別の常駐プロセスを使う（互いに相手の問い合わせを読めず、接続口を奪い合わない）。
    pub fn endpoint() -> String {
        super::override_endpoint().unwrap_or_else(|| {
            format!(
                r"\\.\pipe\okfast-v{}-{}",
                crate::daemon::protocol::VERSION,
                super::user_tag()
            )
        })
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    /// 待ち受け。
    #[derive(Debug)]
    pub struct Listener {
        name: Vec<u16>,
        pending: HANDLE,
    }

    // SAFETY: HANDLE はプロセス内のどのスレッドからでも使える。Listener は accept を
    // &mut self で受けるので、同時に 2 つのスレッドが pending を触ることはない。
    unsafe impl Send for Listener {}

    fn create(name: &[u16], first: bool) -> io::Result<HANDLE> {
        // 所有者（OW）と SYSTEM だけに全権。継承しない（P）
        let sddl = wide("D:P(A;;GA;;;OW)(A;;GA;;;SY)");
        let mut sd: PSECURITY_DESCRIPTOR = null_mut();
        // SAFETY: sddl は NUL 終端の UTF-16。sd には確保された記述子が入り、下で LocalFree する。
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut sd,
                null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        let sa = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd,
            bInheritHandle: 0,
        };
        let mut open_mode = PIPE_ACCESS_DUPLEX;
        if first {
            open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
        }
        // SAFETY: name は NUL 終端の UTF-16、sa は呼び出しの間生きている。
        let h = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                open_mode,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                1 << 16,
                1 << 16,
                0,
                &sa,
            )
        };
        let err = io::Error::last_os_error();
        // SAFETY: sd は ConvertStringSecurityDescriptorToSecurityDescriptorW が確保したもの。
        unsafe { LocalFree(sd) };
        if h == INVALID_HANDLE_VALUE {
            return Err(err);
        }
        Ok(h)
    }

    impl Listener {
        /// 待ち受けを始める。ほかの常駐プロセスがいれば失敗する。
        pub fn bind() -> io::Result<Self> {
            let name = wide(&endpoint());
            let pending = create(&name, true)?;
            Ok(Listener { name, pending })
        }

        /// 接続を 1 つ受ける。
        pub fn accept(&mut self) -> io::Result<Conn> {
            let h = self.pending;
            // SAFETY: h は CreateNamedPipeW が返した有効なハンドル。同期モードで待つ。
            let ok = unsafe { ConnectNamedPipe(h, null_mut()) };
            // SAFETY: 直前の API 呼び出しのエラーを読むだけ。
            if ok == 0 && unsafe { GetLastError() } != ERROR_PIPE_CONNECTED {
                let e = io::Error::last_os_error();
                // SAFETY: h は有効。接続に失敗したので切り離して、同じ実体で待ち直す。
                unsafe { DisconnectNamedPipe(h) };
                return Err(e);
            }
            // 次の接続のための実体を先に作る（接続が重なっても「見つからない」にしない）
            self.pending = create(&self.name, false)?;
            // SAFETY: h は有効で、所有権を File に移す（ほかでは閉じない）。
            Ok(unsafe { File::from_raw_handle(h as _) })
        }

        /// 入れ替わりの確認と片付けに使う印。
        pub fn guard(&self) -> Guard {
            Guard
        }
    }

    /// 入れ替わりの確認と片付けに使う印（Windows では名前を奪われないので何もしない）。
    #[derive(Debug, Clone)]
    pub struct Guard;

    impl Guard {
        /// まだ自分が接続口の持ち主か。
        pub fn is_current(&self) -> bool {
            true
        }

        /// 片付け（パイプはプロセスの終了で消える）。
        pub fn cleanup(&self) {}
    }

    /// 応答を書き終えた接続を閉じる。相手が読み終えるまで待ってから切る。
    pub fn finish(conn: Conn) {
        let h = conn.as_raw_handle() as HANDLE;
        // SAFETY: h は conn が持つ有効なハンドル。conn はこのあと閉じる。
        unsafe {
            FlushFileBuffers(h);
            DisconnectNamedPipe(h);
        }
        drop(conn);
    }

    /// 常駐プロセスにつなぐ。いなければ `NotFound`。
    pub fn connect() -> io::Result<Conn> {
        let name = endpoint();
        let wname = wide(&name);
        for _ in 0..50 {
            match std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&name)
            {
                Ok(f) => return Ok(f),
                Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                    // SAFETY: wname は NUL 終端の UTF-16。
                    unsafe { WaitNamedPipeW(wname.as_ptr(), 20) };
                }
                Err(e) => return Err(e),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "常駐プロセスが混んでいる",
        ))
    }
}

#[cfg(unix)]
mod unix {
    use std::io;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;
    use std::time::Duration;

    /// 接続。
    pub type Conn = UnixStream;

    fn dir() -> PathBuf {
        match std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
            Some(d) => PathBuf::from(d).join("okfast"),
            None => {
                let home = std::env::var_os("HOME").unwrap_or_else(|| "/tmp".into());
                PathBuf::from(home)
                    .join(".cache")
                    .join("okfast")
                    .join("run")
            }
        }
    }

    /// 接続口のパス（`OKFAST_DAEMON_ENDPOINT` で変えられる。試験用）。やりとりの形式の版を含めるので、
    /// 形式の違う okfast は別の常駐プロセスを使う。
    pub fn endpoint() -> String {
        super::override_endpoint().unwrap_or_else(|| {
            dir()
                .join(format!("daemon-v{}.sock", crate::daemon::protocol::VERSION))
                .to_string_lossy()
                .into_owned()
        })
    }

    /// 待ち受け。
    #[derive(Debug)]
    pub struct Listener {
        inner: UnixListener,
        path: PathBuf,
        ino: u64,
    }

    impl Listener {
        /// 待ち受けを始める。ほかの常駐プロセスがいれば失敗する。
        pub fn bind() -> io::Result<Self> {
            let path = PathBuf::from(endpoint());
            if let Some(d) = path.parent() {
                std::fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(d)?;
                std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700))?;
            }
            if UnixStream::connect(&path).is_ok() {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    "常駐プロセスはすでに動いている",
                ));
            }
            let _ = std::fs::remove_file(&path);
            let inner = UnixListener::bind(&path)?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
            let ino = std::fs::metadata(&path)?.ino();
            Ok(Listener { inner, path, ino })
        }

        /// 接続を 1 つ受ける。
        pub fn accept(&mut self) -> io::Result<Conn> {
            let (s, _) = self.inner.accept()?;
            s.set_read_timeout(Some(Duration::from_secs(30)))?;
            Ok(s)
        }

        /// 入れ替わりの確認と片付けに使う印。
        pub fn guard(&self) -> Guard {
            Guard {
                path: self.path.clone(),
                ino: self.ino,
            }
        }
    }

    /// 入れ替わりの確認と片付けに使う印。
    #[derive(Debug, Clone)]
    pub struct Guard {
        path: PathBuf,
        ino: u64,
    }

    impl Guard {
        /// ソケットがまだ自分のものか（同時に起動した別の常駐プロセスに置き換えられていないか）。
        pub fn is_current(&self) -> bool {
            std::fs::metadata(&self.path).is_ok_and(|m| m.ino() == self.ino)
        }

        /// ソケットを消す（自分のものなら）。
        pub fn cleanup(&self) {
            if self.is_current() {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }

    /// 応答を書き終えた接続を閉じる。
    pub fn finish(conn: Conn) {
        drop(conn);
    }

    /// 常駐プロセスにつなぐ。いなければ `NotFound` か `ConnectionRefused`。
    pub fn connect() -> io::Result<Conn> {
        let s = UnixStream::connect(endpoint())?;
        s.set_read_timeout(Some(Duration::from_secs(120)))?;
        Ok(s)
    }
}
