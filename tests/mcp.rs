//! MCP サーバー: ツールの出力が CLI の grep・search・aliases と同じであること。

use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bundle")
}

fn okfast(args: &[&str], cache: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_okfast"));
    c.arg("-b")
        .arg(fixture())
        .args(args)
        .env("OKFAST_DAEMON", "0")
        .env("OKFAST_CACHE_DIR", cache);
    c
}

#[test]
fn tools_match_cli_output() {
    let cache = std::env::temp_dir().join(format!("okfast-mcp-test-{}", std::process::id()));
    let msgs = [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"grep","arguments":{"terms":["Widget","sprocket"],"max_per_page":1}}}"#,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"grep","arguments":{"terms":["Widget"],"pages":true,"all":true}}}"#,
        r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"aliases","arguments":{"terms":["Widget"]}}}"#,
        r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"grep","arguments":{"terms":["存在しない語ですよ"]}}}"#,
        r#"{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"search","arguments":{"terms":["Widget","sprocket"],"top":1,"all":true}}}"#,
        r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"search","arguments":{"terms":["Widget"],"path":["notes/**","glossary/**"],"json":true}}}"#,
    ];
    let mut child = okfast(&["mcp"], &cache)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(msgs.join("\n").as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    let resp: Vec<Value> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(resp.len(), 7);
    let text = |i: usize| {
        resp[i]["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let cli =
        |args: &[&str]| String::from_utf8(okfast(args, &cache).output().unwrap().stdout).unwrap();
    assert_eq!(text(1), cli(&["grep", "Widget", "sprocket", "-m", "1"]));
    assert_eq!(text(2), cli(&["grep", "Widget", "--pages", "--all"]));
    assert_eq!(text(3), cli(&["aliases", "Widget"]));
    // 当たりなしはエラーにしない
    assert_eq!(resp[4]["result"]["isError"], false);
    assert!(text(4).contains("当たり: 0 行"), "{}", text(4));
    assert_eq!(
        text(5),
        cli(&["search", "Widget", "sprocket", "-n", "1", "--all"])
    );
    assert_eq!(
        text(6),
        cli(&[
            "search",
            "Widget",
            "--path",
            "notes/**",
            "--path",
            "glossary/**",
            "--json"
        ])
    );
    let _ = std::fs::remove_dir_all(&cache);
}
