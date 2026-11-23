//! 常駐プロセスとのやりとりの形式。
//!
//! 1 回の接続で、要求を 1 つ送り、応答を 1 つ受け取る。
//! どちらも「長さ（u32 LE）＋中身」の 1 フレーム。中身の文字列・バイト列も「長さ（u32 LE）＋バイト」。

use crate::command::{Command, GrepOptions};
use std::io::{self, Read, Write};
use std::path::PathBuf;

/// 形式の版。変えたら上げる。
pub const VERSION: u32 = 5;
const MAGIC_REQ: &[u8; 4] = b"OKFQ";
const MAGIC_RES: &[u8; 4] = b"OKFR";
/// 1 フレームの上限（壊れた入力で大きなメモリを取らないため）。
const MAX_FRAME: usize = 1 << 30;

/// 要求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// 送った側のビルドの識別子。常駐プロセスと違えば、常駐プロセスを入れ替える。
    pub build_id: String,
    /// 中身。
    pub body: RequestBody,
}

/// 要求の中身。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestBody {
    /// 生きているかの確認。
    Ping,
    /// 終了させる。
    Shutdown,
    /// 状態を聞く。
    Status,
    /// コマンドを実行する。
    Run {
        /// バンドルの root（絶対パス）。
        root: PathBuf,
        /// 設定ファイル（絶対パス。省略時は root の okfast.toml）。
        config: Option<PathBuf>,
        /// 内訳を標準エラーに出す。
        stats: bool,
        /// コマンド。
        command: Command,
    },
}

/// 応答。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    /// 実行した。
    Done {
        /// 終了コード。
        code: u8,
        /// 標準出力。
        stdout: Vec<u8>,
        /// 標準エラー。
        stderr: Vec<u8>,
    },
    /// 常駐プロセスでは扱わない（大きすぎるなど）。呼んだ側で実行する。
    Fallback(String),
    /// ビルドが違う。常駐プロセスを入れ替える。
    WrongBuild(String),
    /// Ping・Shutdown・Status への返事。
    Info(String),
}

struct Enc(Vec<u8>);

impl Enc {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn bytes(&mut self, b: &[u8]) {
        self.u32(b.len() as u32);
        self.0.extend_from_slice(b);
    }
    fn str(&mut self, s: &str) {
        self.bytes(s.as_bytes());
    }
    fn strs(&mut self, v: &[String]) {
        self.u32(v.len() as u32);
        for s in v {
            self.str(s);
        }
    }
    fn path(&mut self, p: &std::path::Path) {
        self.str(&p.to_string_lossy());
    }
}

struct Dec<'a>(&'a [u8]);

fn bad() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "壊れた要求・応答")
}

impl<'a> Dec<'a> {
    fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        if self.0.len() < n {
            return Err(bad());
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }
    fn u8(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn bool(&mut self) -> io::Result<bool> {
        Ok(self.u8()? != 0)
    }
    fn u32(&mut self) -> io::Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn bytes(&mut self) -> io::Result<Vec<u8>> {
        let n = self.u32()? as usize;
        Ok(self.take(n)?.to_vec())
    }
    fn str(&mut self) -> io::Result<String> {
        String::from_utf8(self.bytes()?).map_err(|_| bad())
    }
    fn strs(&mut self) -> io::Result<Vec<String>> {
        let n = self.u32()? as usize;
        (0..n).map(|_| self.str()).collect()
    }
}

fn write_frame(w: &mut impl Write, magic: &[u8; 4], body: &[u8]) -> io::Result<()> {
    let mut buf = Vec::with_capacity(12 + body.len());
    buf.extend_from_slice(magic);
    buf.extend_from_slice(&VERSION.to_le_bytes());
    buf.extend_from_slice(&(body.len() as u32).to_le_bytes());
    buf.extend_from_slice(body);
    w.write_all(&buf)?;
    w.flush()
}

fn read_frame(r: &mut impl Read, magic: &[u8; 4]) -> io::Result<Vec<u8>> {
    let mut head = [0u8; 12];
    r.read_exact(&mut head)?;
    if &head[..4] != magic {
        return Err(bad());
    }
    let ver = u32::from_le_bytes([head[4], head[5], head[6], head[7]]);
    if ver != VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("形式の版が違う（{ver}、期待は {VERSION}）"),
        ));
    }
    let n = u32::from_le_bytes([head[8], head[9], head[10], head[11]]) as usize;
    if n > MAX_FRAME {
        return Err(bad());
    }
    let mut body = vec![0u8; n];
    r.read_exact(&mut body)?;
    Ok(body)
}

impl Request {
    /// 書き出す。
    pub fn write(&self, w: &mut impl Write) -> io::Result<()> {
        let mut e = Enc(Vec::new());
        e.str(&self.build_id);
        match &self.body {
            RequestBody::Ping => e.u8(0),
            RequestBody::Shutdown => e.u8(1),
            RequestBody::Status => e.u8(2),
            RequestBody::Run {
                root,
                config,
                stats,
                command,
            } => {
                e.u8(3);
                e.path(root);
                match config {
                    None => e.u8(0),
                    Some(c) => {
                        e.u8(1);
                        e.path(c);
                    }
                }
                e.u8(u8::from(*stats));
                match command {
                    Command::Grep { terms, opts } => {
                        e.u8(0);
                        e.strs(terms);
                        for b in [
                            opts.no_aliases,
                            opts.all,
                            opts.count,
                            opts.files,
                            opts.pages,
                            opts.json,
                        ] {
                            e.u8(u8::from(b));
                        }
                        for n in [
                            opts.max_per_page,
                            opts.max_pages,
                            opts.context,
                            opts.max_columns,
                            opts.max_chars,
                        ] {
                            e.u32(u32::try_from(n).unwrap_or(u32::MAX));
                        }
                    }
                    Command::Aliases { terms, json } => {
                        e.u8(1);
                        e.strs(terms);
                        e.u8(u8::from(*json));
                    }
                }
            }
        }
        write_frame(w, MAGIC_REQ, &e.0)
    }

    /// 読み込む。
    pub fn read(r: &mut impl Read) -> io::Result<Self> {
        let buf = read_frame(r, MAGIC_REQ)?;
        let mut d = Dec(&buf);
        let build_id = d.str()?;
        let body = match d.u8()? {
            0 => RequestBody::Ping,
            1 => RequestBody::Shutdown,
            2 => RequestBody::Status,
            3 => {
                let root = PathBuf::from(d.str()?);
                let config = match d.u8()? {
                    0 => None,
                    _ => Some(PathBuf::from(d.str()?)),
                };
                let stats = d.bool()?;
                let command = match d.u8()? {
                    0 => Command::Grep {
                        terms: d.strs()?,
                        opts: GrepOptions {
                            no_aliases: d.bool()?,
                            all: d.bool()?,
                            count: d.bool()?,
                            files: d.bool()?,
                            pages: d.bool()?,
                            json: d.bool()?,
                            max_per_page: d.u32()? as usize,
                            max_pages: d.u32()? as usize,
                            context: d.u32()? as usize,
                            max_columns: d.u32()? as usize,
                            max_chars: d.u32()? as usize,
                        },
                    },
                    1 => Command::Aliases {
                        terms: d.strs()?,
                        json: d.bool()?,
                    },
                    _ => return Err(bad()),
                };
                RequestBody::Run {
                    root,
                    config,
                    stats,
                    command,
                }
            }
            _ => return Err(bad()),
        };
        Ok(Request { build_id, body })
    }
}

impl Response {
    /// 書き出す。
    pub fn write(&self, w: &mut impl Write) -> io::Result<()> {
        let mut e = Enc(Vec::new());
        match self {
            Response::Done {
                code,
                stdout,
                stderr,
            } => {
                e.u8(0);
                e.u8(*code);
                e.bytes(stdout);
                e.bytes(stderr);
            }
            Response::Fallback(s) => {
                e.u8(1);
                e.str(s);
            }
            Response::WrongBuild(s) => {
                e.u8(2);
                e.str(s);
            }
            Response::Info(s) => {
                e.u8(3);
                e.str(s);
            }
        }
        write_frame(w, MAGIC_RES, &e.0)
    }

    /// 読み込む。
    pub fn read(r: &mut impl Read) -> io::Result<Self> {
        let buf = read_frame(r, MAGIC_RES)?;
        let mut d = Dec(&buf);
        Ok(match d.u8()? {
            0 => Response::Done {
                code: d.u8()?,
                stdout: d.bytes()?,
                stderr: d.bytes()?,
            },
            1 => Response::Fallback(d.str()?),
            2 => Response::WrongBuild(d.str()?),
            3 => Response::Info(d.str()?),
            _ => return Err(bad()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let reqs = [
            Request {
                build_id: "x".into(),
                body: RequestBody::Ping,
            },
            Request {
                build_id: "y".into(),
                body: RequestBody::Run {
                    root: PathBuf::from("/a/b"),
                    config: Some(PathBuf::from("/c.toml")),
                    stats: true,
                    command: Command::Grep {
                        terms: vec!["Widget|WP".into(), "検索".into()],
                        opts: GrepOptions {
                            all: true,
                            files: true,
                            json: true,
                            context: 2,
                            max_pages: 0,
                            ..GrepOptions::default()
                        },
                    },
                },
            },
            Request {
                build_id: String::new(),
                body: RequestBody::Run {
                    root: PathBuf::from("r"),
                    config: None,
                    stats: false,
                    command: Command::Aliases {
                        terms: vec!["a".into()],
                        json: true,
                    },
                },
            },
        ];
        for r in reqs {
            let mut buf = Vec::new();
            r.write(&mut buf).unwrap();
            assert_eq!(Request::read(&mut buf.as_slice()).unwrap(), r);
        }
        let ress = [
            Response::Done {
                code: 1,
                stdout: b"out".to_vec(),
                stderr: vec![],
            },
            Response::Fallback("big".into()),
            Response::WrongBuild("b".into()),
            Response::Info("i".into()),
        ];
        for r in ress {
            let mut buf = Vec::new();
            r.write(&mut buf).unwrap();
            assert_eq!(Response::read(&mut buf.as_slice()).unwrap(), r);
        }
    }

    #[test]
    fn rejects_garbage() {
        assert!(Request::read(&mut &b"OKFQ\x01\0\0\0\x05\0\0\0abc"[..]).is_err());
        assert!(Request::read(&mut &b"XXXX"[..]).is_err());
    }
}
