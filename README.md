# okfast

**OKF 0.2 を最高速で取り扱うための Rust 製ツールキット**

okfast は OKF 0.2 形式のデータを、読み込み・検証・変換・書き出しまで可能な限り高速に処理することを目的としたライブラリ兼 CLI です。

> ステータス: 開発初期。実装済みは、同義語による言い換えつきの検索（`okfast grep` / `okfast aliases`）、読む範囲（見出しの節）を BM25 の順位で返す検索（`okfast search`）、それを速く答える常駐プロセス、AI のクライアントから直接呼べる MCP サーバー（`okfast mcp`）、検索の設定を作る・確かめる道具（`okfast config`）。API は予告なく変わります。

## 目標

- **速度最優先** — ゼロコピー解析、アロケーション最小化、ストリーミング処理、必要に応じて SIMD / 並列化
- **仕様準拠** — OKF 0.2 仕様に厳密に従い、不正入力は明確なエラー位置付きで報告
- **ライブラリ + CLI** — Rust から `okfast` クレートとして利用でき、単体の `okfast` コマンドとしても使える
- **計測駆動** — すべての最適化はベンチマークで効果を確認してから採用

## インストール

[GitHub Releases](https://github.com/nenaaki/okfast/releases) から、使う環境のファイルを取ってきて展開し、`okfast`（Windows は `okfast.exe`）を PATH の通ったディレクトリに置きます。

| 環境 | ファイル |
|---|---|
| Windows (x64) | `okfast-<版>-x86_64-pc-windows-msvc.zip` |
| Linux・WSL (x64) | `okfast-<版>-x86_64-unknown-linux-musl.tar.gz`（静的リンク。glibc の版によらず動く） |

```sh
# Linux・WSL の例
tar xzf okfast-<版>-x86_64-unknown-linux-musl.tar.gz
install -m 755 okfast-<版>-x86_64-unknown-linux-musl/okfast ~/.local/bin/
okfast --version
```

展開したフォルダの `skills/` に、Claude Code 用のスキルが入っています（[AI に使わせる](#ai-に使わせるgrep-より優先させる) を参照）。`SHA256SUMS.txt` でファイルを確かめられます。

更新するときは、新しい実行ファイルで置き換えるだけです。常駐プロセスは次の問い合わせで新しい版に入れ替わります。

ソースから入れる場合は `cargo install --git https://github.com/nenaaki/okfast`（Rust 1.85 以上）。

確かめた環境は Windows 11 と WSL2（Ubuntu 24.04）です。macOS は試していません。

リリースの作り方（開発者向け）: `Cargo.toml` の `version` を上げてコミットし、`v<版>` のタグ（例: `v0.1.0`）を push すると、GitHub Actions がテストを回して Windows 版と Linux 版を作り、Releases に置きます（`.github/workflows/release.yml`）。

## 必要環境（開発）

- Rust stable（`rust-toolchain.toml` で固定。rustup が自動で取得します）
- コンポーネント: `rustfmt`, `clippy`, `rust-src`

```sh
# rustup 未導入の場合（Windows）
winget install Rustlang.Rustup
```

Windows で GNU ツールチェーン（`x86_64-pc-windows-gnu`）を使う場合は、`windows-sys` などのビルドに MinGW-w64 の `dlltool` が必要です。

```sh
winget install BrechtSanders.WinLibs.POSIX.UCRT   # インストール後、ターミナルを再起動
```

## ビルドと実行

```sh
cargo build --release          # 最適化ビルド (LTO fat, codegen-units=1)
cargo run --release -- <args>  # CLI 実行
cargo test                     # テスト
cargo bench                    # ベンチマーク (criterion, target/criterion/report/index.html)
```

ローカルマシン専用にさらに速くしたい場合:

```sh
RUSTFLAGS="-C target-cpu=native" cargo build --release
```

## 使い方

```sh
okfast -b <バンドルの root> grep "Widget Pro"           # 同義語の辞書で言い換えて検索する
okfast -b <root> grep "部品表|BOM" 在庫                # | は自分で並べる言い換え。語を複数並べると、どれかに当たる行を出す
okfast -b <root> grep --no-aliases WP                  # 言い換えない
okfast -b <root> grep --all ウィジェットプロ             # 設定の exclude を無視してすべてのページを検索する
okfast -b <root> grep -c WP / grep -l WP              # ページごとの行数だけ / パスだけを出す（順位順）
okfast -b <root> grep -C 2 -m 20 WP                   # 前後 2 行つき、1 ページ 20 行まで
okfast -b <root> grep --full WP                       # 上限なしですべて出す
okfast -b <root> grep --json WP                       # 1 行の JSON で出す（プログラム・試験用）
okfast -b <root> search "Widget Pro" 価格              # 読む範囲（見出しの節）を関連度の順に返す（下の「読む範囲を返す検索」）
okfast -b <root> aliases "Widget Pro" [--json]        # 何に言い換えられるかだけを表示する
okfast -b <root> config init > okfast.toml           # バンドルを走査して設定の下書きを作る
okfast -b <root> config check                        # 設定をバンドルに当てて確かめる（警告なしで終了コード 0）
okfast config reference                              # 設定の書式の説明
okfast -b <root> mcp                                 # MCP サーバーとして動く（下の「MCP サーバー」）
```

出力は、AI が 1 回の呼び出しで次に読むページを決められる形にしています。

- ページを順位付けして上から出します。順位は、主題のページ（辞書が指すページ）→ 当たった語の種類が多い → 題名に当たった → 見出し（`#` の行）に当たった → 当たった行が多い、の順です
- 各ページの前に `# ページ: パス「題名」 N 行・語 a/b` の行が付き、続けて `パス:行番号:内容` を出します。`-C N` で前後の行も出します（前後の行は `パス-行番号-内容`、離れた塊のあいだは `--`）
- 既定では 1 ページ 5 行（`-m`）、30 ページ（`--max-pages`）、1 行 300 文字（`--max-columns`）、全体で約 1 万文字（`--max-chars`）までにします。出さなかった行は行番号で、出さなかったページはパスと行数の一覧で示します。`--full` で上限なし
- 全体の上限は、Claude Code が長い出力をファイルに逃がす（読み直しに手間が増える）のを避けるためです
- 先頭の `#` 行に、言い換えの内容とその出所（`glossary/別名表.md:12` のようなページと行）、主題のページ、件数が出ます
- 終了コードは、当たりありが 0、当たりなしが 1、エラーが 2 です

### JSON の出力

`grep --json` は、結果を 1 行の JSON で出します。検索だけの試験（順位・行番号の比べ合わせ）や、ほかのプログラムから使うためのものです。

```json
{"terms":[{"term":"Widget","added":["WP"],"conditional":[{"when":"path=references/**","names":["ウィジェト"]}],"boost":["notes/widget.md"],"lines":12}],
 "total_lines":12,"total_pages":3,"searched":40,"excluded":2,"exclude":["glossary/**"],
 "pages":[{"rank":1,"path":"notes/widget.md","title":"Widget Pro","lines":8,"terms":1,"title_hit":true,"heading_hits":1,"boosted":true,
           "hits":[{"line":5,"text":"WP の価格は…"}]}]}
```

- `pages` は当たったページを順位順にすべて並べます。`hits`（当たった行）は、上位 `--max-pages` ページに 1 ページ `-m` 行まで付けます（`lines` は省く前の行数）。行は `--max-columns` で切ります。`--full` で全部
- `terms[].lines` は語ごとの当たった行の数、`terms` はそのページで当たった語の種類の数、`boosted` は主題のページ
- `-C`・`-c`・`-l`・`--pages` とは一緒に使えません。`--max-chars` は使いません

### 読む範囲を返す検索（`okfast search`）

`grep` は当たった行を返し、どこからどこまでを Read するかは AI が決めます。けれども、答えの行そのものには検索語が入っていないことが多く（同じ節の別の行に語がある）、当たった行の周りだけを読むと取りこぼします。`search` は、結果を **読む範囲（ページ:開始行-終了行）** で返します。

```sh
okfast -b <root> search "Widget Pro" 価格          # 関連度の上位 8 範囲（1 ページ 2 範囲まで）
okfast -b <root> search -n 5 --per-page 1 価格     # 上位 5 範囲、1 ページ 1 範囲まで
okfast -b <root> search --since 2026-01-01 価格    # その日以降の日付の範囲だけ
okfast -b <root> search --sort date 価格           # 日付の新しい順
okfast -b <root> search --recent 0 価格            # 新しい記録を足さない
okfast -b <root> search --path "meetings/**" 価格  # この glob のページだけ（複数可。exclude のページも含める）
okfast -b <root> search --json 価格                # 1 行の JSON
```

```text
# search: Widget Pro ／ 価格
# 主題のページ（点を上げる）: notes/widget.md
# 当たり: 12 行・6 範囲 / 3 ページ（検索 40 ページ）
## 必ず読む範囲（見出しや決まったページで選んだもの。関連度によらない）
notes/widget.md:18-21［§概要 > 未決｜credibility: A］
  （語の当たりはない）
## 関連度の上位
notes/widget.md:10-16［2026-03-02｜§概要 > 価格｜credibility: A］
  10: ## 価格
  11: 価格は次のとおり。
## 新しい記録（関連度の上位に入らなかったもの）
notes/meeting.md:6-7［2026-04-01｜§議題］
  7: WP の価格について、値下げの声が出た。
# ほかの範囲（順位順）: notes/a.md:3-9, …
```

- **結果の単位は節**です。節は見出し（`#`〜`######`）の行から次の見出しの手前まで。答えの行に語がなくても、同じ節に当たりがあれば範囲に入ります。40 行（`[search.sections] max_lines`）を超える節は、当たった行の前後 4 行（`window`）だけを切り出し、`節 a-b の一部` と示します
- **順位は BM25** です。行を 1 つの文書とみなし、ありふれた語は軽く、珍しい語は重く、短い行ほど重く数えます（行の中の回数は数えない）。脚注の定義の行（`[^id]: 出典`）とリンクの行き先（`](...)` の中）は当たりに数えません
- **主題のページは点を上げます**。題名（`[search] topic_keys` で説明・タグなども足せる）か辞書の boost に当たった語の idf の割合 `share` を取り、そのページの行の点に `1 + share` を掛けます。全部の語に当たるページほど強く上がり、ありふれた 1 語だけが当たるページが上位を占めることはありません
- 範囲の点は、当たった行の点を高い順に並べて 1・1/2・1/4… を掛けて足したもの（`[search.sections] score = "decay"`）。何行にもわたって当たる節が、1 行だけかする節より上に来ます。並びは点の高い順（同点なら日付の新しい順）です
- **新しい記録**: 上位のあとに、日付の新しい範囲を 4 件（`[search.recent] count`）足します。行の日付をどこから取るか（表の行頭・frontmatter・脚注の出典・見出し）は `date_sources` で宣言します。宣言がなければ足しません。表の行頭の年のない日付（`MM-DD`）は、ページの `generated.at` の年で読みます
- **必ず読む範囲**: 主題のページ（`share` の高い順）の上位 2 件（`[search.must_read] topics`）で、見出しが設定の語（`headings`）に当たる節を、関連度によらず先頭に出します。決まったページ（`pages`）の上位の範囲も足せます。1 行 1 件の表なら `window = 0` で当たった行だけを出せます
- 範囲の見出しの `［…］` には、日付・`§見出し`（親の見出しが 1 つあれば `§親 > 見出し`）・`[search] show_keys` に書いた frontmatter の値が入ります
- `--path <glob>` で、検索するページを絞れます（複数可）。`exclude` に当たるページでも、`--path` で明示すれば含めます
- 抜粋は点の高い当たった行 2 行です。範囲は Read の `offset` = 開始行、`limit` = 終了行 - 開始行 + 1 で読みます
- 終了コードは、範囲ありが 0、なしが 1、エラーが 2 です

設定の書き方は [docs/config.md](docs/config.md) の `[search.sections]`・`[search.recent]`・`[search.must_read]` にあります。

`search --json` は、範囲ごとの当たった行と点を返します。バンドル側のスクリプトが、ここに独自の情報を足して表示することを想定しています。

```json
{"terms":[{"term":"価格","added":[],"conditional":[],"boost":[],"lines":4}],
 "total_lines":4,"total_ranges":3,"total_pages":2,"searched":2,"excluded":0,"exclude":[],"paths":[],
 "topics":[{"rel":"notes/widget.md","topic":1.0}],
 "ranges":[{"rel":"notes/widget.md","from":10,"to":16,"title":"Widget Pro","heading":"価格","headings":["概要","価格"],
            "section":[10,16],"partial":false,"date":"2026-03-02","meta":{"credibility":"A"},"score":4.038,"terms":1,
            "boosted":false,"topic":1.0,"must":false,"recent":false,
            "hits":[{"line":10,"text":"## 価格","score":4.038,"date":null}]}],
 "rest":[{"rel":"notes/a.md","from":3,"to":9,"score":0.5,"date":null}]}
```

- `ranges` は出す範囲を、必ず読む範囲（`must`）→ 上位 → 新しい記録（`recent`）の順に並べます。`rest` は出さなかった範囲（順位順）
- `section` は節全体の行、`partial` は節の一部を切り出したか、`meta` は `show_keys` の値です。`hits` は範囲の中の当たった行すべて（1 行 300 文字まで）
- `headings` は節の見出しの階層（いちばん上から）、`heading` はその最後の見出しです。`topic` はページの主題の度合い `share`（0〜1）、`boosted` は辞書の boost のリンク先か。`topics` は主題のページを `share` の高い順に並べたもの、`paths` は `--path` の glob です

### 常駐プロセス

`grep`・`search`・`aliases` は、まず常駐プロセスに問い合わせます。常駐プロセスは、バンドルのページ・正規化済みの本文・同義語の辞書をすべてメモリに持ち、ファイルの変更を OS の通知で監視しています。いなければその回は自分で実行し、常駐プロセスを裏で起動します（次の回から速くなります）。

- 変更は、問い合わせに答える前にすべて反映します。ページの追加・変更・削除、ディレクトリの追加・名前の変更、`okfast.toml` の変更を、書いた直後の問い合わせから反映します
- 本文の合計が上限（既定 256 MiB、`OKFAST_DAEMON_MAX_MB`）を超えるバンドルは扱わず、自分で実行します
- しばらく使われなければ終わります（既定 30 分、`OKFAST_DAEMON_IDLE_SECS`）
- 答えが上限（既定 30 秒、`OKFAST_DAEMON_TIMEOUT_SECS`）までに来なければ、その回は自分で実行し、固まった常駐プロセスを終わらせます（次の回に起動し直します）
- 接続口は、Windows では所有者だけが接続できる名前付きパイプ、Linux では `$XDG_RUNTIME_DIR/okfast/daemon-v<形式の版>.sock`（0700 のディレクトリの中）です。ポートは開けません。名前にやりとりの形式の版を含めるので、形式の違う版の okfast が同時に使われても、それぞれ別の常駐プロセスを使います
- 実行ファイルを作り直すと、次の問い合わせで常駐プロセスを入れ替えます。Windows では実行中のファイルを上書きできないため、`%LOCALAPPDATA%\okfast\daemon\` に置いた複製を動かします
- `--no-daemon`（または `OKFAST_DAEMON=0`）で使わない。`okfast daemon status | stop | start` で状態を見る・終わらせる・先に起動する。`--stats` で常駐プロセスで実行したかと時間が出ます
- WSL から `/mnt/c` の下のファイルを扱う使い方は対象外です（変更の通知が届かず、読み込みも遅い）

### キャッシュ

読み込んだページは 1 つのキャッシュファイルにまとめ、次回からは変わったファイル（サイズか更新日時が違うもの）だけを読み直します。Windows ではファイルを開くたびのコストが大きいため、これで読み込みが約 6 分の 1 になります（約 300 ページ・10MB のバンドルで 158ms → 27ms）。

- 置き場所: `OKFAST_CACHE_DIR`、なければ `%LOCALAPPDATA%\okfast`（Linux などは `$XDG_CACHE_HOME/okfast` か `~/.cache/okfast`）。バンドルの中には何も書きません
- `--no-cache` で使わない、`--cache-dir <dir>` で場所を変える、`--stats` で読み込みの内訳を標準エラーに出す
- キャッシュが壊れていたり形式が古かったりしたら、黙って作り直します。書けなくても検索は続けます

### 同義語の設定（`okfast.toml`）

OKF 0.2 には同義語の決まりがない（type も frontmatter のキーも自由）ため、同義語をどこからどう取り出すかを、バンドルの root に置く `okfast.toml`（または `--config`）で宣言します。設定がなければ言い換えはしません。

- 取り出し元は2種類あります。表（`[[aliases.table]]`: 1 行 = 1 概念、呼び名の列の語どうしを同義語にする）と、frontmatter（`[[aliases.frontmatter]]`: title と別名のキーの値を同義語にする）です
- 特定のページを検索するときだけ使う列（文字起こしの誤認識など）、取り出し元を frontmatter の値で絞る設定、既定で検索しないページも書けます
- 言い換えは**連鎖させません**。問いの語が一致した概念の呼び名だけを足します

書式の説明は [docs/config.md](docs/config.md)（`okfast config reference` でも表示）、設定例は [examples/glossary-table.okfast.toml](examples/glossary-table.okfast.toml) にあります。

### MCP サーバー

`okfast -b <root> mcp` で、標準入出力の MCP サーバーとして動きます。AI のクライアント（Claude Code など）は、`grep`・`search`・`aliases` をツールとして直接呼べます。

- シェルを通さないので、呼び出しのたびのシェルの起動・フック・許可の確認がかかりません（Windows では 1 回あたり数秒になることがあります）
- Bash を持たないエージェント（Read・Grep・Glob だけのものなど）にも、ツールとして渡せます
- ツールの引数は CLI のオプションと同じ名前です（`terms`、`pages`、`files`、`count`、`json`、`context`、`max_per_page`、`max_pages`、`max_columns`、`max_chars`、`full`、`all`、`no_aliases`）。search は `terms`、`top`、`per_page`、`recent`、`since`、`sort`（`relevance` か `date`）、`path`（glob の配列）、`json`、`all`、`no_aliases`。出力も CLI と同じです
- 検索は CLI と同じく常駐プロセスを使います（`--no-daemon` を付ければ使いません）。ファイルの変更はすぐに反映します

Claude Code では、バンドルを持つリポジトリの `.mcp.json` に書きます（`okfast` に PATH が通っていなければ、実行ファイルのパスを書きます）。

```json
{
  "mcpServers": {
    "okfast": { "command": "okfast", "args": ["-b", "knowledge", "mcp"] }
  }
}
```

ツールは `mcp__okfast__grep`・`mcp__okfast__search`・`mcp__okfast__aliases` という名前になります。`.claude/settings.json` に次のように書くと、確認なしで使わせられ、`.mcp.json` のサーバーを使うかの確認も出ません。

```json
{
  "permissions": { "allow": ["mcp__okfast"] },
  "enabledMcpjsonServers": ["okfast"]
}
```

- MCP サーバーは会話のあいだ動き続けます。Windows では動いている実行ファイルを置き換えられないので、会話中に okfast を更新する仕組みがあるなら、実行ファイルの複製を MCP サーバーとして動かします（常駐プロセスは自分で複製から動くので、気にしなくてよい）

### AI に使わせる（Grep より優先させる）

Claude Code は、何も言わなければ Grep ツール（中身は ripgrep）でファイルを探します。okfast を優先させるには、バンドルを持つリポジトリで次の 3 つを行います。MCP サーバーを使う場合は、2 の「`okfast -b knowledge grep`」「`okfast -b knowledge search`」を「okfast の MCP ツール（`mcp__okfast__grep`・`mcp__okfast__search`）」に読み替え、3 は上の `mcp__okfast` の許可にします。

**1. スキルを置く**

[skills/okfast-search/SKILL.md](skills/okfast-search/SKILL.md) を、バンドルを持つリポジトリの `.claude/skills/okfast-search/SKILL.md` にコピーします。ナレッジを調べる場面でスキルが選ばれ、使い方（出力の読み方、当たらないときの探し直し方、okfast を使わない場面）が AI に渡ります。`okfast` コマンドに PATH が通っている必要があります。

**2. CLAUDE.md に書く（いちばん効きます）**

スキルは AI が「使うべき」と判断したときにしか読まれません。毎回の会話で読まれる CLAUDE.md に、はっきり書いておきます。`knowledge/` の部分はバンドルの root に合わせます。

```markdown
## ナレッジの検索

- `knowledge/` の中を語で探すときは、Grep ツール・rg・grep を使わず `okfast -b knowledge grep <語>...` を使う（okfast-search スキル）
- 問いに答えるために読む範囲を決めたいときは `okfast -b knowledge search <語>...` を使う。結果の `パス:開始行-終了行` をそのまま Read する
- okfast は同義語の辞書で言い換え、表記の揺れを吸収する。自分で同義語を `|` で並べなくてよい
- Grep を使ってよいのは、正規表現が必要なとき、`.md` 以外のファイルを探すとき、okfast がエラーになったときだけ
```

**3. 許可を与える**

実行のたびに確認を求められると、AI は確認の要らない Grep に流れがちです。`.claude/settings.json` で okfast の検索を許可しておきます。

```json
{
  "permissions": {
    "allow": ["Bash(okfast grep:*)", "Bash(okfast search:*)", "Bash(okfast aliases:*)", "Bash(okfast -b:*)"]
  }
}
```

会話の中で頼むなら、「ナレッジは okfast で探して」「Grep ではなく okfast grep を使って」のように道具の名前を挙げると確実です。それでも Grep を使うようなら、Claude Code のフック（PreToolUse）で、バンドルのパスに対する Grep を止めて okfast を使うよう返す方法もあります。

### AI に設定を書かせる

`okfast config init` で下書きを作り、`okfast config check` が警告なしになるまで直す、という流れを想定しています。check は、当たらないページ・見出しと合わない列名・リンク切れ・複数の概念にある語などを、直し方の手がかり（似た見出し、実際の見出し、該当しそうなページ）とともに返します。

Claude Code 用のスキルを [skills/okfast-config/SKILL.md](skills/okfast-config/SKILL.md) に同梱しています。バンドルを持つリポジトリの `.claude/skills/okfast-config/` にコピーして使います。

## ディレクトリ構成

```
src/bundle.rs       バンドルの読み込み（予約名 index.md・log.md と . で始まる名前を除く。frontmatter は参照時に解析）
src/cache.rs        読み込んだページのキャッシュ
src/frontmatter.rs  frontmatter の切り出しと参照（未知のキーも保持）
src/markdown.rs     表・リンク・脚注の軽量な解析
src/selector.rs     ページの選び方（path の glob ＋ 任意の frontmatter キー）
src/config.rs       okfast.toml
src/aliases.rs      同義語の辞書（取り出しの内訳も残す）
src/check.rs        設定の確かめ（okfast config check）
src/init.rs         設定の下書き（okfast config init）
src/textindex.rs    検索用に正規化した本文（ページごとに 1 回だけ作る）
src/search.rs       行単位の検索（memmem / Teddy で当たりそうな行を探し、その行だけ全部の語を確かめる）
src/sections.rs     本文を見出しで節に分ける（ページごとに 1 回だけ作る）
src/dates.rs        行の日付の取り出し（設定の date_sources に従う）
src/ranges.rs       読む範囲を返す検索（節にまとめ、BM25 で順位を付け、新しい記録・必ず読む範囲を足す）
src/command.rs      grep・search・aliases の実行と出力（CLI と常駐プロセスで共有）
src/mcp.rs          MCP サーバー（標準入出力の JSON-RPC。ツールは grep・search・aliases）
src/daemon/         常駐プロセス（mod.rs: 起動と問い合わせ、state.rs: メモリ上の状態と監視、ipc.rs: 接続口、protocol.rs: 形式）
src/main.rs         CLI
examples/           設定例・段階ごとの時間計測（timing.rs）
benches/            criterion ベンチマーク
docs/config.md      okfast.toml の書式（バイナリに埋め込み、okfast config reference で表示）
docs/spec/          OKF 0.2 仕様メモ・参照資料
skills/             Claude Code 用のスキル（okfast-search: 検索、okfast-config: 設定）
tests/              結合テスト（fixtures/bundle が試験用バンドル）
```

## ライセンス

MIT — [LICENSE](LICENSE) を参照。
