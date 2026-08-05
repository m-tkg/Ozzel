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

## 結果（Phase 3 後、中央値）

Phase 3 で入れた変更:

1. **`VirtualDir` エントリ一覧キャッシュ**（`src/virtual_dir.rs`）: アーカイブの生エントリ一覧（`RawEntry` 全件）を `VirtualDir`（`Rc<RefCell<Option<CachedEntries>>>`）に保持し、`(mtime, len)` が変わらない限り descend/go_parent/`Pane::reload` のたびの再オープン・再パース・再解凍を排除。zip は central directory の再パース、tar 系はストリームの再走査、`.tar.xz` はアーカイブ全体の再インフレートをそれぞれ回避する。`VirtualDir::clone`（`Pane::virtual_go_parent` が使う）はキャッシュを共有（`Rc` のクローンのみ）するので、archive 内を潜っても登っても同じキャッシュを使い続ける。アーカイブファイル自体が置き換わった場合（mtime/len 変化）は再読込。`stat` 自体が失敗する場合（アーカイブ削除後など）は既存キャッシュをそのまま使う（一覧のための stat 失敗を「変化なし」として扱う）。
2. **コピーの高速化**（`src/tasks/copy_move.rs`）: `WHOLE_FILE_COPY_THRESHOLD`（4 MiB）以下のファイルは `std::fs::copy` 一発（OS の clonefile/copy_file_range が効き、permissions もコピーされる）。それを超えるファイルは従来通りチャンク読み書き（`TransferCtx::buf` に1回だけ確保した 1 MiB バッファを全ファイルで再利用、per-chunk のキャンセルチェック・進捗報告を維持）し、完了後に明示的に `src` の permissions を `dest` にコピーして両経路の挙動を揃えた。
3. **起動時の初回描画**（`src/main.rs`/`src/app.rs`）: `App::new_unloaded` で両ペインを空のまま構築 → `run` の先頭で `terminal.draw` を1回実行 → `App::load_initial_dirs` で実際のディレクトリ読込 → 通常ループ、という順序に変更。遅いマウントや巨大ディレクトリでも alternate screen に入った直後に画面が出る。`App::new`（既存テスト・`test_app` が使う経路）自体の外部挙動は変えていない（`new_unloaded` + `load_initial_dirs` の合成のまま、eager load を維持）。

`archive_listing_cached/*` が今回の新規ベンチ（Phase 0〜2 に対応ベンチなし）— `VirtualDir::list` を一度呼んでキャッシュを温めた後、同じインスタンスに対して繰り返し呼ぶ「実際にアプリ内で descend/go_parent するたびに通る」経路を計測したもの。`archive_listing/*`（`read_archive_dir_entries` 直呼び、`VirtualDir` を経由しない常にコールド経路）は Phase 3 で一切変更していない対照群として残してある。実行環境は上記「実行環境」節と同一マシン・同一プロファイル（rustc 1.94.0、criterion 0.8.2）。

| ベンチ | 対象 | Phase 2 後 中央値 | Phase 3 後 中央値 | 変化 |
|---|---|---|---|---|
| `archive_listing/zip_100_entries` | 常にコールド（`read_archive_dir_entries` 直呼び、対照） | 280.15 µs | 273.69 µs | ノイズ内（-2.3%） |
| `archive_listing/tar_gz_100_entries` | 同上（対照） | 101.92 µs | 98.875 µs | ノイズ内（-3.0%） |
| `archive_listing/tar_xz_100_entries` | 同上（対照、Phase 2 に対応ベンチなし） | — | 93.515 µs | — |
| `archive_listing_cached/zip_100_entries/cache_hit` | `VirtualDir::list` キャッシュヒット（Phase 2 に対応ベンチなし） | — | 12.891 µs | 参考: コールド比 約 -95.3%（1/21） |
| `archive_listing_cached/tar_gz_100_entries/cache_hit` | 同上 | — | 12.866 µs | 参考: コールド比 約 -87.0%（1/7.7） |
| `archive_listing_cached/tar_xz_100_entries/cache_hit` | 同上 | — | 12.849 µs | 参考: コールド比 約 -86.3%（1/7.3） |
| `pane_visible_entries/1000_files/Name` | 未変更コード（対照） | 1.5003 ms | 1.2954 ms | ノイズ内（-13.6%、実行間ブレ） |
| `pane_visible_entries/10000_files/Name` | 未変更コード（対照） | 20.319 ms | 15.774 ms | ノイズ内（-22.4%、実行間ブレ） |
| `pane_visible_entries_cached/10000_files/cache_hit` | 未変更コード（対照） | 3.2402 µs | 3.2054 µs | 変化なし |
| `wrap_log_lines_500_lines_width_80` | 未変更コード（対照） | 540.72 µs | 521.85 µs | ノイズ内（-3.5%） |
| `wrap_log_lines_tail_500_lines_width_80_need_4_rows` | 未変更コード（対照） | 2.5848 µs | 2.5264 µs | ノイズ内（-2.3%） |

所見:

- `archive_listing_cached/*` は3フォーマットとも約 12.8〜12.9 µs で横並び — キャッシュヒット後は `group_children`（メモリ内の `HashMap` フィルタ）のコストが支配的で、元のフォーマット差（zip の central directory 再パース vs tar 系のストリーム再走査 vs xz の全展開）が完全に消えている。これは狙い通り: descend/go_parent/`reload` の毎回のコストが、フォーマットに関係なく「小さな一定コスト」に潰れた。
- コールド経路（`archive_listing/*`）に対する削減率は zip が最大（約 1/21）、tar_gz/tar_xz が約 1/7〜1/8。zip はそもそも central directory の再パースコストが tar 系の逐次ストリーム走査より重く、キャッシュの効果がそのまま比率に出ている。
- `.tar.xz` はこのベンチのアーカイブサイズ（100エントリ・ペイロード17バイトずつ）では全展開のコストがまだ小さいため、削減率自体は tar_gz と大差ない。プランで問題視されていた「2GBのtar.xzで1階層降りるだけで2GB再解凍」のような巨大アーカイブほど、キャッシュ有無の差は絶対時間で見て桁違いに開く（全展開コストがアーカイブサイズに比例するのに対し、キャッシュヒット後のコストはエントリ数にしか依存しないため）。
- `pane_visible_entries/*`・`wrap_log_lines*` は Phase 3 で一切触れていない対照群。今回の実行では前回計測比で軒並み改善方向に振れているが、コード変更のない箇所なので Phase 1/2 の記録同様この日の実行間ノイズ（マシン負荷差）と判断し、Phase 3 の効果としては扱わない。`pane_visible_entries_cached/10000_files/cache_hit` はノイズの影響が最も小さい参照点として「実質変化なし」を確認する用途で見るとよい。
- コピー高速化（`src/tasks/copy_move.rs`）と起動時初回描画（`main.rs`/`app.rs`）は criterion の単発関数呼び出しベンチでは測りにくい種類の変更（前者は `std::fs::copy` の OS 側最適化次第、後者はイベントループ全体の見た目の挙動）。正しさは `cargo test` の新規テスト（`copy_preserves_source_permissions_on_both_the_small_and_chunked_paths`、`run_copy_of_a_large_file_goes_through_the_chunked_path_and_finishes_ok`、`new_unloaded_builds_both_panes_with_no_entries_and_no_io` 等）で担保。体感確認は `cargo run --release` での手動操作のみ（巨大ファイルコピー時の alloc 削減、起動直後に空ペインがすぐ表示されること）。

### 設計判断メモ

- **xz の展開済みバッファ保持について**: 一覧のためだけの再解凍はキャッシュで解消したが、単一ファイルの抽出/ビューア表示経路（`extract_single_from_tar`、ひいては `.tar.xz` の場合は `open_tar_archive` の全展開）は今回のスコープでは従来通り毎回再解凍する。理由: (1) 展開済みバッファを `VirtualDir` に保持すると、巨大な `.tar.xz`（プラン記載の2GB級）をブラウズしているだけでその全体をメモリに常駐させ続けることになり、「一覧のための再解凍を避ける」という目的に対してメモリコストが不釣り合いに大きい。(2) 単一ファイルの抽出はユーザー操作（ビューアで開く/展開する）のたびに1回起きるだけで、一覧のように「ディレクトリ移動のたびに毎回」ではないため、頻度あたりのコスト削減効果が一覧キャッシュよりずっと小さい。以上より「一覧はキャッシュ、単一抽出は従来通り」で本フェーズの目標（ブラウズ操作のたびの重い再デコードの排除）は達成していると判断し、単一抽出側のキャッシュ化は将来課題として見送った。
- **コピー閾値（`WHOLE_FILE_COPY_THRESHOLD` = 4 MiB）について**: 進捗バーの更新間隔（`PROGRESS_MIN_INTERVAL` = 100ms）を下回る時間で完了するファイルは、そもそもチャンク単位の進捗報告が画面に反映される機会がほとんどない。4 MiB は目安として「一般的なディスク/SSD速度なら100ms以内に転送が終わる規模」を想定した値。この閾値以下は `std::fs::copy` 一発（cancel チェックはファイル開始前のみ）、超える場合は従来通りチャンクループ（cancel チェックはチャンク毎、進捗報告も継続）とし、大量の小ファイルコピー（例: 10万ファイル）で `vec![0u8; 1MiB]` の zeroed alloc がファイル数分発生していた問題を解消しつつ、大きい単一ファイルのコピー中に体感できるキャンセル応答性・進捗表示は変えていない。
