//! MCP（Model Context Protocol）サーバー。標準入出力で JSON-RPC 2.0 を 1 行 1 メッセージでやりとりする。
//!
//! AI のクライアント（Claude Code など）が okfast をツールとして直接呼べるようにする。シェルを通さないので、
//! シェルの起動・フック・許可の確認のぶんだけ速い。ツールは `grep`・`search`・`aliases` で、出力は CLI と同じ。
//! 実行のしかた（常駐プロセスを使うか）は呼び出し側が `run` で渡す。

use crate::command::{Command, GrepOptions, SearchOptions};
use serde_json::{Value, json};
use std::io::{self, BufRead, Write};

/// 対応するプロトコルの版（新しい順）。クライアントが求めた版がこの中にあればそれで答える。
const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// コマンドを実行して、終了コードと出力を返す関数。終了コード 2 以上はツールのエラーとして返す
/// （grep の当たりなしの 1 はエラーにしない）。
pub type Runner<'a> = dyn FnMut(&Command) -> crate::Result<(u8, String)> + 'a;

/// `input` から読み、`output` に書く。入力が終わったら戻る。
///
/// `root` はバンドルの root の表示用（ツールの説明に、パスの起点として書く）。
pub fn serve(
    input: impl BufRead,
    mut output: impl Write,
    root: &str,
    run: &mut Runner<'_>,
) -> io::Result<()> {
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(resp) = handle(&line, root, run) {
            serde_json::to_writer(&mut output, &resp)?;
            output.write_all(b"\n")?;
            output.flush()?;
        }
    }
    Ok(())
}

/// 1 メッセージを処理する。通知（`id` なし）には何も返さない。
fn handle(line: &str, root: &str, run: &mut Runner<'_>) -> Option<Value> {
    let msg: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            return Some(error(
                Value::Null,
                -32700,
                &format!("JSON として読めない: {e}"),
            ));
        }
    };
    let id = msg.get("id").cloned();
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let id = id?; // 通知・応答には返さない
    if method.is_empty() {
        return Some(error(id, -32600, "method がない"));
    }
    let result = match method {
        "initialize" => Ok(initialize(&params, root)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools(root) })),
        "tools/call" => call(&params, run),
        _ => Err((-32601, format!("知らないメソッド: {method}"))),
    };
    Some(match result {
        Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
        Err((code, m)) => error(id, code, &m),
    })
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn initialize(params: &Value, root: &str) -> Value {
    let asked = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or("");
    let version = PROTOCOL_VERSIONS
        .iter()
        .find(|v| **v == asked)
        .unwrap_or(&PROTOCOL_VERSIONS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "okfast", "version": env!("CARGO_PKG_VERSION") },
        "instructions": format!(
            "OKF バンドル（{root}）の .md を語で探すときは、Grep・rg・grep ではなく okfast の grep ツールを使う。\
             同義語の辞書で言い換え、全角・半角・大文字小文字・空白の違いを吸収し、主題のページを先に順位順で返す。\
             答えを探して読む範囲（ページ:開始行-終了行）を決めたいときは search ツールを使う。\
             結果のパスはバンドルの root からの相対パス。続きは行番号で Read する。"
        ),
    })
}

fn tools(root: &str) -> Value {
    let n = |d: &str, default: usize| json!({"type": "integer", "minimum": 0, "description": format!("{d}（既定 {default}、0 は無制限）")});
    json!([
        {
            "name": "grep",
            "title": "okfast grep",
            "description": format!(
                "OKF バンドル（{root}）の .md を語で探す。Grep の代わりに使う。同義語の辞書で自動的に言い換え、\
                 表記の揺れ（全角・半角・大文字小文字・空白）を吸収する。語を複数渡すと、どれかに当たる行を探し、\
                 多くの語に当たるページほど上に並べる。語の中の | は言い換え（例: \"見積|見積もり\"）。\
                 出力は CLI の okfast grep と同じで、パスはバンドルの root からの相対パス（パス:行番号:内容）。\
                 既定では上位のページから行数・文字数を絞って返す。当たったページを漏れなく見るには pages。"
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "terms": {"type": "array", "items": {"type": "string"}, "minItems": 1, "description": "検索語"},
                    "pages": {"type": "boolean", "description": "当たったページをすべて 1 ページ 1 行（題名・行数・抜粋）で出す"},
                    "files": {"type": "boolean", "description": "当たったページのパスだけを順位順に出す"},
                    "count": {"type": "boolean", "description": "ページごとの行数だけを出す"},
                    "json": {"type": "boolean", "description": "結果を 1 行の JSON で出す（プログラム用）"},
                    "context": {"type": "integer", "minimum": 0, "description": "当たった行の前後に出す行の数（既定 0）"},
                    "max_per_page": n("1 ページに出す当たった行の数", GrepOptions::MAX_PER_PAGE),
                    "max_pages": n("行を出すページの数", GrepOptions::MAX_PAGES),
                    "max_columns": n("1 行に出す文字数", GrepOptions::MAX_COLUMNS),
                    "max_chars": n("行の部分全体の文字数の目安", GrepOptions::MAX_CHARS),
                    "full": {"type": "boolean", "description": "上限なしですべて出す"},
                    "all": {"type": "boolean", "description": "設定の exclude を無視してすべてのページを探す"},
                    "no_aliases": {"type": "boolean", "description": "辞書で言い換えない"}
                },
                "required": ["terms"]
            },
            "annotations": {"readOnlyHint": true, "openWorldHint": false}
        },
        {
            "name": "search",
            "title": "okfast search",
            "description": format!(
                "OKF バンドル（{root}）で、問いに答えるために読む範囲を探す。結果の 1 件は見出しで区切った節で、\
                 `パス:開始行-終了行［日付｜§見出し］` と当たった行の抜粋。答えの行に語がなくても、同じ節に当たりがあれば\
                 範囲に入る。順位は BM25（珍しい語ほど重い）で、題名などに語が入る主題のページは点が上がる。必ず読む範囲（設定した見出しの節）と、\
                 日付の新しい範囲も足す。範囲は Read の offset・limit でそのまま読む。言い換えは grep と同じ。"
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "terms": {"type": "array", "items": {"type": "string"}, "minItems": 1, "description": "検索語"},
                    "top": n("関連度の上位から出す範囲の数", SearchOptions::TOP),
                    "per_page": n("1 ページから出す範囲の数", SearchOptions::PER_PAGE),
                    "recent": {"type": "integer", "minimum": 0, "description": "上位のあとに足す、日付の新しい範囲の数（既定は設定の [search.recent] count）"},
                    "since": {"type": "string", "description": "この日付（YYYY-MM-DD）より前の範囲と、日付の分からない範囲を出さない"},
                    "sort": {"type": "string", "enum": ["relevance", "date"], "description": "並べ方（既定 relevance）"},
                    "json": {"type": "boolean", "description": "結果を 1 行の JSON で出す（プログラム用）"},
                    "all": {"type": "boolean", "description": "設定の exclude を無視してすべてのページを探す"},
                    "path": {"type": "array", "items": {"type": "string"}, "description": "この glob に当たるページだけを探す（exclude に当たるページも含める。例: [\"meetings/**\"]）"},
                    "body": {"type": "integer", "minimum": 0, "description": "必ず読む範囲と、関連度の上位のこの数の範囲の本文（行番号つき）を返す（既定 0 は返さない）。返した範囲は Read しなくてよい"},
                    "body_lines": {"type": "integer", "minimum": 0, "description": format!("返す本文の合計行数の上限（既定 {}）。超えた範囲から下は抜粋だけ（Read で読む）", SearchOptions::BODY_LINES)},
                    "timeline": {"type": "integer", "minimum": 0, "description": "返した範囲の日付つきの行の時系列の一覧に出す行の数（既定は設定の [search.timeline] max。0 は出さない）"},
                    "no_aliases": {"type": "boolean", "description": "辞書で言い換えない"}
                },
                "required": ["terms"]
            },
            "annotations": {"readOnlyHint": true, "openWorldHint": false}
        },
        {
            "name": "aliases",
            "title": "okfast aliases",
            "description": "語が同義語の辞書でどう言い換えられるか（足す呼び名・条件つきの呼び名・主題のページ）を見る。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "terms": {"type": "array", "items": {"type": "string"}, "minItems": 1, "description": "語"}
                },
                "required": ["terms"]
            },
            "annotations": {"readOnlyHint": true, "openWorldHint": false}
        }
    ])
}

fn call(params: &Value, run: &mut Runner<'_>) -> Result<Value, (i64, String)> {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let empty = json!({});
    let args = params.get("arguments").unwrap_or(&empty);
    let cmd = match name {
        "grep" => Command::Grep {
            terms: terms(args)?,
            opts: grep_options(args)?,
        },
        "search" => Command::Search {
            terms: terms(args)?,
            opts: search_options(args)?,
        },
        "aliases" => Command::Aliases {
            terms: terms(args)?,
            json: false,
        },
        _ => return Err((-32602, format!("知らないツール: {name}"))),
    };
    Ok(match run(&cmd) {
        Ok((code, text)) => {
            json!({"content": [{"type": "text", "text": text}], "isError": code >= 2})
        }
        Err(e) => {
            json!({"content": [{"type": "text", "text": format!("okfast: {e}")}], "isError": true})
        }
    })
}

fn terms(args: &Value) -> Result<Vec<String>, (i64, String)> {
    let v: Vec<String> = args
        .get("terms")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if v.is_empty() {
        return Err((-32602, "terms（検索語の配列）がない".to_string()));
    }
    Ok(v)
}

fn grep_options(args: &Value) -> Result<GrepOptions, (i64, String)> {
    let flag = |k: &str| args.get(k).and_then(Value::as_bool).unwrap_or(false);
    let num = |k: &str, default: usize| -> Result<usize, (i64, String)> {
        match args.get(k) {
            None | Some(Value::Null) => Ok(default),
            Some(v) => v
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .ok_or((-32602, format!("{k} は 0 以上の整数"))),
        }
    };
    let full = flag("full");
    let lim = |k: &str, default: usize| -> Result<usize, (i64, String)> {
        let n = num(k, default)?;
        Ok(if full { 0 } else { n })
    };
    Ok(GrepOptions {
        no_aliases: flag("no_aliases"),
        all: flag("all"),
        count: flag("count"),
        files: flag("files"),
        pages: flag("pages"),
        json: flag("json"),
        max_per_page: lim("max_per_page", GrepOptions::MAX_PER_PAGE)?,
        max_pages: lim("max_pages", GrepOptions::MAX_PAGES)?,
        context: num("context", 0)?,
        max_columns: lim("max_columns", GrepOptions::MAX_COLUMNS)?,
        max_chars: lim("max_chars", GrepOptions::MAX_CHARS)?,
    })
}

fn search_options(args: &Value) -> Result<SearchOptions, (i64, String)> {
    let flag = |k: &str| args.get(k).and_then(Value::as_bool).unwrap_or(false);
    let num = |k: &str| -> Result<Option<usize>, (i64, String)> {
        match args.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => v
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .map(Some)
                .ok_or((-32602, format!("{k} は 0 以上の整数"))),
        }
    };
    let text = |k: &str| -> Result<Option<String>, (i64, String)> {
        match args.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(_) => Err((-32602, format!("{k} は文字列"))),
        }
    };
    let sort_date = match text("sort")?.as_deref() {
        None | Some("relevance") => false,
        Some("date") => true,
        Some(s) => return Err((-32602, format!("sort は relevance か date（「{s}」）"))),
    };
    Ok(SearchOptions {
        no_aliases: flag("no_aliases"),
        all: flag("all"),
        json: flag("json"),
        top: num("top")?.unwrap_or(SearchOptions::TOP),
        per_page: num("per_page")?.unwrap_or(SearchOptions::PER_PAGE),
        recent: num("recent")?,
        since: text("since")?,
        sort_date,
        body: num("body")?.unwrap_or(0),
        body_lines: num("body_lines")?.unwrap_or(SearchOptions::BODY_LINES),
        timeline: num("timeline")?,
        paths: match args.get("path") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::String(s)) => vec![s.clone()],
            Some(Value::Array(a)) if a.iter().all(Value::is_string) => a
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
            Some(_) => return Err((-32602, "path は glob の文字列か、その配列".to_string())),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn talk(lines: &[&str]) -> (Vec<Value>, Vec<Command>) {
        let mut seen = Vec::new();
        let mut run = |c: &Command| -> crate::Result<(u8, String)> {
            seen.push(c.clone());
            Ok((0, "結果".to_string()))
        };
        let input = lines.join("\n");
        let mut out = Vec::new();
        serve(input.as_bytes(), &mut out, "/b", &mut run).unwrap();
        let resp = String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        (resp, seen)
    }

    #[test]
    fn handshake_list_and_call() {
        let (r, seen) = talk(&[
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"grep","arguments":{"terms":["見積"],"pages":true,"max_chars":500}}}"#,
            r#"{"jsonrpc":"2.0","id":"x","method":"tools/call","params":{"name":"aliases","arguments":{"terms":["a"]}}}"#,
        ]);
        assert_eq!(r.len(), 4, "通知には返さない");
        assert_eq!(r[0]["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(r[0]["result"]["serverInfo"]["name"], "okfast");
        let names: Vec<&str> = r[1]["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["grep", "search", "aliases"]);
        assert_eq!(r[2]["result"]["content"][0]["text"], "結果");
        assert_eq!(r[2]["result"]["isError"], false);
        assert_eq!(r[3]["id"], "x");
        match &seen[0] {
            Command::Grep { terms, opts } => {
                assert_eq!(terms, &["見積"]);
                assert!(opts.pages);
                assert_eq!(opts.max_chars, 500);
                assert_eq!(opts.max_per_page, GrepOptions::MAX_PER_PAGE);
            }
            c => panic!("{c:?}"),
        }
        assert!(matches!(&seen[1], Command::Aliases { json: false, .. }));
    }

    #[test]
    fn errors() {
        let (r, seen) = talk(&[
            "{not json",
            r#"{"jsonrpc":"2.0","id":1,"method":"nope"}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"grep","arguments":{}}}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"grep","arguments":{"terms":["a"],"max_pages":-1}}}"#,
            r#"{"jsonrpc":"2.0","id":4,"method":"initialize","params":{"protocolVersion":"1999-01-01"}}"#,
            r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"grep","arguments":{"terms":["a"],"full":true}}}"#,
        ]);
        assert_eq!(r[0]["error"]["code"], -32700);
        assert_eq!(r[1]["error"]["code"], -32601);
        assert_eq!(r[2]["error"]["code"], -32602);
        assert_eq!(r[3]["error"]["code"], -32602);
        assert_eq!(r[4]["result"]["protocolVersion"], PROTOCOL_VERSIONS[0]);
        match &seen[..] {
            [Command::Grep { opts, .. }] => {
                assert_eq!(
                    (opts.max_per_page, opts.max_pages, opts.max_chars),
                    (0, 0, 0)
                );
            }
            s => panic!("{s:?}"),
        }
    }

    #[test]
    fn run_error_is_tool_error() {
        let mut run = |_: &Command| -> crate::Result<(u8, String)> {
            Err(crate::Error::Query("壊れている".into()))
        };
        let input = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"aliases","arguments":{"terms":["a"]}}}"#;
        let mut out = Vec::new();
        serve(input.as_bytes(), &mut out, "/b", &mut run).unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["result"]["isError"], true);
    }
}
