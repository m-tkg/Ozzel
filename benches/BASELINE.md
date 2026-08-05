# ベンチマーク ベースライン

このファイルはリファクタリング期間中の before/after 追跡用。数値はローカルマシン依存（下記の実行環境でしか意味を持たない絶対値ではなく、同一環境での相対比較に使うこと）。再計測する場合は `cargo bench` を実行し、`Benchmarking <name>` の直後に出る `time: [下限 中央値 上限]` の中央値をこの表に転記する。

## 実行環境

- 日付: 2026-08-05
- マシン: Apple M3 Pro (arm64, macOS Darwin 27.0.0)
- rustc: 1.94.0 (4a4ef493e 2026-03-02, Homebrew)
- ビルドプロファイル: `cargo bench`（release + `lto = "thin"` + `codegen-units = 1"`）
- criterion 0.8.2

## 結果（Phase 0 ベースライン、中央値）

| ベンチ | 対象 | 中央値 |
|---|---|---|
| `pane_visible_entries/1000_files/Name` | `Pane::new` + `visible_entries()`、1,000ファイル、ソートキー Name | 2.5231 ms |
| `pane_visible_entries/1000_files/Ext` | 同上、ソートキー Ext | 3.0284 ms |
| `pane_visible_entries/10000_files/Name` | 同上、10,000ファイル、ソートキー Name | 33.305 ms |
| `pane_visible_entries/10000_files/Ext` | 同上、10,000ファイル、ソートキー Ext | 40.049 ms |
| `wrap_log_lines_500_lines_width_80` | `ui::log_view::wrap_log_lines`、合成500行（タイムスタンプ+日本語含む可変長メッセージ）、幅80 | 650.70 µs |
| `archive_listing/zip_100_entries` | `virtual_dir::read_archive_dir_entries`、zip・100エントリ（ネスト付き） | 274.90 µs |
| `archive_listing/tar_gz_100_entries` | 同上、tar.gz・100エントリ（ネスト付き） | 101.67 µs |

各ベンチの生ログ（外れ値情報含む）は `cargo bench` の標準出力を参照。criterion は加えて `target/criterion/` 以下にHTMLレポートと生データ（`estimates.json` 等）を保存するので、次回以降の `cargo bench` はそれと自動比較して "Performance has improved/regressed" を出力する。

## アイドル CPU の手動計測手順

自動化された計測ベンチではなく、実機での体感確認用の手順。

1. リリースビルドを用意する。

   ```sh
   cargo build --release
   ```

2. リリースバイナリを起動し、何も操作せず放置する。

   ```sh
   ./target/release/ozzel
   ```

3. 別のターミナルを開き、起動した ozzel の PID を確認する。

   ```sh
   pgrep -x ozzel
   ```

4. その PID に対して `ps` で `%cpu` を数回（例: 5〜10回、1〜2秒間隔で）サンプリングし、平均を取る。

   ```sh
   for i in $(seq 1 10); do ps -o %cpu= -p <pid>; sleep 1; done
   ```

   ワンライナーで平均まで出す場合:

   ```sh
   for i in $(seq 1 10); do ps -o %cpu= -p <pid>; sleep 1; done | awk '{s+=$1; n++} END {print s/n "%"}'
   ```

5. 得られた平均値をこのファイルに追記し、前後のリファクタリングで比較する。イベントループが `Duration::from_millis(50)` のポーリング間隔で回っている（`main.rs` の `run` 関数）ため、何もしていない間もこのポーリング周期に応じたCPU消費が発生する — 削減余地があるとすればここが対象になる。
