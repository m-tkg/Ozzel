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

## 結果（Phase 1 後、中央値）

Phase 1 で入れた変更: `FsEntry::name_lower`/`ext_lower` の事前計算（`compare_entries`/`FilterSpec::matches`/`jump_matches` の per-call alloc を除去）、`Pane::visible_entries` のソート済みインデックスキャッシュ（`Rc<Vec<usize>>`、`invalidate_visible_cache` で明示的に無効化）、`Pane::selected_entry` 統合アクセサ、`main.rs` の dirty flag（`App::needs_redraw`）+ アイドル時ポーリング間隔延長（50ms → 250ms）。実行環境は上記「実行環境」節と同一マシン・同一プロファイル（rustc 1.94.0、criterion 0.8.2）。

`pane_visible_entries/*` は Phase 0 と同じ計測方法（毎イテレーション `Pane::new` からやり直す、常にキャッシュミス経路）。新規追加した `pane_visible_entries_cached/*` は `Pane` を `b.iter` の外で一度だけ作り、以降は常にキャッシュヒット経路を計測したもの — 実際のアプリでカーソル移動・描画のたびに繰り返し呼ばれるのはこちら側に近い。

| ベンチ | 対象 | Phase 0 中央値 | Phase 1 後 中央値 | 変化 |
|---|---|---|---|---|
| `pane_visible_entries/1000_files/Name` | キャッシュミス（`Pane::new` 毎回）、1,000ファイル、Name | 2.5231 ms | 1.3949 ms | -44.7% |
| `pane_visible_entries/1000_files/Ext` | 同上、Ext | 3.0284 ms | 1.4024 ms | -54.2% |
| `pane_visible_entries/10000_files/Name` | キャッシュミス、10,000ファイル、Name | 33.305 ms | 18.454 ms | -44.4% |
| `pane_visible_entries/10000_files/Ext` | 同上、Ext | 40.049 ms | 18.394 ms | -53.9% |
| `pane_visible_entries_cached/1000_files/cache_hit` | キャッシュヒット、1,000ファイル（Phase 0 に対応ベンチなし） | — | 342.87 ns | 参考: ミス経路比 約 -99.98%（1/4070） |
| `pane_visible_entries_cached/10000_files/cache_hit` | キャッシュヒット、10,000ファイル（Phase 0 に対応ベンチなし） | — | 3.2360 µs | 参考: ミス経路比 約 -99.98%（1/5700） |
| `wrap_log_lines_500_lines_width_80` | 変更なし（比較用の対照ベンチ） | 650.70 µs | 654.94 µs | ノイズ内（+0.4%） |
| `archive_listing/zip_100_entries` | `read_archive_dir_entries`（`FsEntry::name_lower`/`ext_lower` 計算が1エントリにつき1回増加） | 274.90 µs | 280.19 µs | +1.8%（ほぼノイズ、想定内） |
| `archive_listing/tar_gz_100_entries` | 同上 | 101.67 µs | 100.58 µs | -1.7%（ノイズ内） |

所見:

- キャッシュミス経路（`pane_visible_entries/*`）は `compare_entries`/`extension_lower` の per-comparison alloc がなくなったことで Phase 0 比 約44〜54% 改善。事前計算のオーバーヘッド（`read_dir_entries` 側で1エントリにつき1回 `lower_keys` を呼ぶ）を含めてもなお大幅改善しており、ミス経路の悪化はない。
- キャッシュヒット経路（`pane_visible_entries_cached/*`）はミス経路比で約4000〜5700倍高速 — 実アプリでの効果はここが本命（カーソル移動・毎フレーム描画・selected_entry系アクセサはほぼ常にヒット経路を通る）。
- `archive_listing/zip_100_entries` の +1.8% は `group_children` 側で追加した `lower_keys` 呼び出し1回分のコストで、ノイズと判別しづらいレベル（tar_gz 側は逆に -1.7% で相殺方向）。ここは元々ホットパスの対象外なので許容。
- dirty flag + ポーリング延長（main.rs の `run` ループ）は criterion ベンチの対象外（イベントループ全体の挙動）。効果は `benches/BASELINE.md` の「アイドル CPU の手動計測手順」で確認すること — 本ラウンドでは自動テスト（`cargo test`）と `cargo run --release` での手動操作確認のみ実施し、アイドル CPU の実測値はまだ追記していない。

## 結果（Phase 2 後、中央値）

Phase 2 で入れた変更: ログ下部パネル（`ui::log_view::render_log_lines`）を全行 wrap → tail slice から、新設の `wrap_log_lines_tail`（末尾行から逆順に wrap し必要行数分だけで打ち切り）へ変更。`LogLine` にタイムスタンプ整形済み文字列（`formatted_timestamp`）を追加し `App::log_push` 時に一度だけ `chrono::format` する（`wrap_log_lines`/`wrap_log_lines_tail` 双方から恩恵、後者だけでなく前者＝フルログビュー/検索の経路も速くなっている）。加えて `App` に `(log_generation, width)` キーのフルログ wrap キャッシュ、`Keymap::generation`（生成時に一意 id を採番するだけ — 生成後は差し替えのみで in-place 変更されないため）キーの help/settings キーバインド行キャッシュ、typed query キーの function-list フィルタ結果キャッシュを追加。settings のカテゴリ一覧・function-list パレットは可視ウィンドウ分だけ `ListItem` を構築するよう変更（オフスクリーン分の `combos_for`/フォーマットを回避）。viewer の `slice_display_cols` は全 ASCII 行なら byte range の直接スライスに短絡（巨大な1行ファイルの横スクロール向け）。`/`/`?` 検索は `Matcher`（コンパイル済み regex）を `ViewerSearch::Active` に `Rc` で保持し、viewer/help/log 3画面とも描画毎の再コンパイルを廃止。filter 入力は生テキストが変わっていなければ `FilterSpec::parse` を再実行しない。実行環境は上記「実行環境」節と同一マシン・同一プロファイル（rustc 1.94.0、criterion 0.8.2）。

`wrap_log_lines_tail_500_lines_width_80_need_4_rows` が今回の新規ベンチ（Phase 0/1 に対応ベンチなし）— ログ下部パネルの典型ケース（500行中、直近4行相当だけ必要）を模したもの。`pane_visible_entries/*` 系は Phase 2 で一切触っていないコード（`pane.rs`）だが、この日の計測で ±5〜11% ほど揺れており（`pane_visible_entries/1000_files/Name` を単体で再計測すると逆方向に -5.4% と出た）、マシン負荷由来のノイズと判断し無視してよい。

| ベンチ | 対象 | Phase 1 後 中央値 | Phase 2 後 中央値 | 変化 |
|---|---|---|---|---|
| `wrap_log_lines_500_lines_width_80` | フルログビュー/検索が通る経路（全500行 wrap）。`formatted_timestamp` 事前計算の効果がそのまま乗る | 654.94 µs | 540.72 µs | -17.4%（chrono format の除去分） |
| `wrap_log_lines_tail_500_lines_width_80_need_4_rows` | ログ下部パネルが実際に通る経路（末尾4行だけ必要）。Phase 0/1 に対応ベンチなし | — | 2.5848 µs | 参考: フル wrap 比 約 -99.5%（1/209） |
| `pane_visible_entries/1000_files/Name` | 未変更コード（対照） | 1.3949 ms | 1.5003 ms（再計測で 1.4366 ms） | ノイズ内（この日の実行間ブレが ±5〜11%） |
| `pane_visible_entries/1000_files/Ext` | 未変更コード（対照） | 1.4024 ms | 1.4187 ms | ノイズ内（+1.2%） |
| `pane_visible_entries/10000_files/Name` | 未変更コード（対照） | 18.454 ms | 20.319 ms | ノイズ内（実行間ブレ、後述） |
| `pane_visible_entries/10000_files/Ext` | 未変更コード（対照） | 18.394 ms | 19.970 ms | ノイズ内（実行間ブレ、後述） |
| `pane_visible_entries_cached/1000_files/cache_hit` | 未変更コード（対照） | 342.87 ns | 347.14 ns | ノイズ内（+1.2%） |
| `pane_visible_entries_cached/10000_files/cache_hit` | 未変更コード（対照） | 3.2360 µs | 3.2402 µs | 変化なし |
| `archive_listing/zip_100_entries` | 未変更コード（対照） | 280.19 µs | 280.15 µs | 変化なし |
| `archive_listing/tar_gz_100_entries` | 未変更コード（対照） | 100.58 µs | 101.92 µs | ノイズ内（+1.3%） |

所見:

- 本フェーズの主目的（ログ下部パネルの毎フレームコスト削減）は `wrap_log_lines_tail` ベンチで直接確認できる: 500行フル wrap 比で約 1/209（99.5%減）。実アプリでは毎フレーム（起動中ずっと）通る経路なので、体感上の効果は `pane_visible_entries` キャッシュヒット化（Phase 1）と並んで大きいはず。
- `wrap_log_lines_500_lines_width_80`（フルログビュー/検索の経路、コード自体は「全行 wrap」のままで変えていない）も -17.4% 改善している — これは `LogLine::formatted_timestamp` の事前計算（`App::log_push` 時に一度だけ `chrono::format`、以降は `wrap_log_lines`/`wrap_log_lines_tail` とも文字列 clone のみ）の効果で、tail-first 化そのものとは別に効いている。
- help/settings のキーバインド行キャッシュ、function-list のフィルタ結果キャッシュ、viewer/help/log の `Matcher` 再利用、filter 入力の再パース抑止は、いずれも「同じフレームを何度も再計算しない」系の変更で、criterion の単発呼び出しベンチでは測りにくい（呼ばれる頻度が減ることが効果の本体であって、1回あたりのコストはほぼ変えていない）。正しさは `cargo test`（キャッシュが実際のキーマップ変更/クエリ変更に追従することを検証する新規テスト込み）で担保 — 対話的な手動操作での体感確認は今回未実施。
- `pane_visible_entries/*`・`pane_visible_entries_cached/*`・`archive_listing/*` は Phase 2 で一切触れていない対照群。`pane_visible_entries/10000_files/*` の Phase 1 比 +8〜10% は、`pane_visible_entries/1000_files/Name` を単体で再計測した際に -5.4% と逆方向に振れたことから、コード変更ではなくこの日の実行間ノイズ（他プロセスの負荷等）と判断した。

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
