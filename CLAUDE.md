# CLAUDE.md

このリポジトリで作業する Claude 向けのガイド。

## プロジェクト概要

okfast は **OKF 0.2 を最高速で取り扱う** ための Rust 製ライブラリ兼 CLI。
判断に迷ったら「仕様に正しく、かつ速い」方を選ぶ。正しさを犠牲にした高速化はしない。

- 対応仕様: OKF 0.2（定数 `okfast::OKF_VERSION`）
- 仕様の一次資料: https://github.com/GoogleCloudPlatform/knowledge-catalog/blob/main/okf/SPEC.md （メモは `docs/spec/`）。
- OKF 0.2 で必須なのは frontmatter の `type` だけで、type の値もキーも自由（§4.1）。**特定のバンドルの約束（層・type 名・辞書の場所など）を本体に書き込まない。** バンドルごとの違いは `okfast.toml` で宣言させる。未知の type・キーで失敗してはならない（§11）。
- 仕様の一次資料はほかにもあれば `docs/spec/` に置く。仕様が不明な点は推測で実装せず、ユーザーに確認する。

## コマンド

```sh
cargo build --release
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all
cargo bench                 # criterion
cargo build --profile profiling   # release 最適化 + デバッグシンボル（プロファイラ用）
```

作業完了前に必ず `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test` を通すこと。

- CI（`.github/workflows/ci.yml`）は Windows と Linux で fmt・clippy・test、加えて Rust 1.85（`rust-version`）での check を回す。依存を足したら `cargo +1.85 check --all-targets` で確かめる（`rustup toolchain install 1.85-x86_64-pc-windows-gnu --profile minimal`）
- リリースは `v<版>` のタグを push すると `.github/workflows/release.yml` が作る（Windows は MSVC、Linux は musl の静的リンク）。タグの前に `Cargo.toml` の `version` を上げる

## 構成と検証

- モジュールの役割は README の「ディレクトリ構成」。同義語の設定の書式の正本は `docs/config.md`（バイナリに埋め込んで `okfast config reference` で出す）
- 設定のキーを足す・変えるときは、`docs/config.md`・`check.rs`（診断）・`init.rs`（下書き）・`skills/okfast-config/SKILL.md` もそろえる。`tests/config_tools.rs` の `reference_documents_every_key` に新しいキーを足す
- grep・aliases のオプションや出力を変えるときは、`skills/okfast-search/SKILL.md` と README の「AI に使わせる」もそろえる
- **リポジトリ（コード・コメント・テスト・ドキュメント・設定例）に、実在のプロジェクト名・顧客名・そのデータ由来の語を書かない。** 例は架空のもの（Widget Pro など）にする。実データでの確認はローカルだけで行う
- 設定例は `examples/glossary-table.okfast.toml`
- 段階ごとの時間: `cargo run --release --example timing -- <bundle> <config> <語>...`（キャッシュなし・初回・2 回目の読み込みと検索）。CLI では `--stats`
- 計測は PowerShell から行う（Git Bash から起動すると 1 回あたり 100ms 以上上乗せされる）。Windows のプロセス起動は揺れが大きいので、CLI 全体の時間より timing の内訳で比べる
- キャッシュの形式を変えたら `cache.rs` の `VERSION` を上げる
- 常駐プロセスとのやりとりの形式を変えたら `daemon/protocol.rs` の `VERSION` を上げる。常駐プロセスで実行できるコマンドを足すときは `command.rs` に置き、CLI と常駐プロセスで同じ出力にする（`tests/daemon.rs` で `--no-daemon` と比べる）
- 計測で CLI 全体の時間を比べるときは、常駐あり・なし（`--no-daemon`）を分けて書く。実データでの比較相手は ripgrep（Claude Code の Grep ツールの中身）
- 試験は `OKFAST_DAEMON_ENDPOINT` と `OKFAST_CACHE_DIR` で、利用者の常駐プロセス・キャッシュと分ける
- WSL では `/mnt/c` の下のバンドルは対象外（変更の通知が届かない）。WSL 用のバイナリは Windows から `cargo build --release --target x86_64-unknown-linux-musl --config 'target.x86_64-unknown-linux-musl.linker="rust-lld"'` で作る（WSL に C コンパイラがないため）

## 環境

- Windows 11 / ツールチェーンは `stable-x86_64-pc-windows-gnu`（`rust-toolchain.toml` で stable 固定）
- MinGW-w64 (WinLibs, winget 導入) の `dlltool` が PATH に必要。`dlltool.exe: program not found` が出たら PATH を確認する
- Edition 2024

## 性能に関する方針

- **計測してから最適化する。** 高速化を主張する変更には、前後の `cargo bench` 結果を添える。
- ホットパスでは:
  - 入力は `&[u8]` / `&str` で受け、可能な限り借用（ゼロコピー）で返す
  - ループ内でのアロケーション・`clone`・`format!` を避ける
  - 境界チェックは iterator や事前の長さ検証で除去する
  - 分岐予測を意識し、エラー処理は `#[cold]` な関数に追い出す
- `unsafe` は計測で効果が確認できた場合のみ。使うときは必ず `// SAFETY:` コメントで根拠を書き、safe 版との一致をテストする。
- 依存クレートの追加は最小限に。追加する場合は理由（速度・正しさ）を説明する。
- `target-cpu=native` はリポジトリ設定に入れない（配布バイナリの互換性のため）。SIMD は実行時検出 (`is_x86_feature_detected!`) か `cfg(target_feature)` で切り替える。

## コード規約

- エラーは独自のエラー型で返し、ライブラリ内で `panic!` / `unwrap()` を使わない（テストは可）。
- エラーには入力上の位置（オフセット／行・列）を含める。
- 公開 API には doc コメントを付ける。
- テスト: 単体テストは各モジュールの `#[cfg(test)]`、仕様準拠テストは `tests/` に置く。

## コミット

- ユーザーに頼まれたときだけコミットする。
- コミットメッセージは変更の要点を簡潔に（日本語可）。
