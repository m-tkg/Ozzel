# Benchmark baseline

This file is for before/after tracking during the refactoring period. The numbers are dependent on the local machine (they are not absolute values meaningful outside the execution environment described below — use them only for relative comparison within the same environment). To remeasure, run `cargo bench` and transcribe the median from the `time: [lower median upper]` line that appears right after `Benchmarking <name>` into this table.

## Execution environment

- Date: 2026-08-05
- Machine: Apple M3 Pro (arm64, macOS Darwin 27.0.0)
- rustc: 1.94.0 (4a4ef493e 2026-03-02, Homebrew)
- Build profile: `cargo bench` (release + `lto = "thin"` + `codegen-units = 1"`)
- criterion 0.8.2

## Results (Phase 0 baseline, median)

| Bench | Target | Median |
|---|---|---|
| `pane_visible_entries/1000_files/Name` | `Pane::new` + `visible_entries()`, 1,000 files, sort key Name | 2.5231 ms |
| `pane_visible_entries/1000_files/Ext` | Same as above, sort key Ext | 3.0284 ms |
| `pane_visible_entries/10000_files/Name` | Same as above, 10,000 files, sort key Name | 33.305 ms |
| `pane_visible_entries/10000_files/Ext` | Same as above, 10,000 files, sort key Ext | 40.049 ms |
| `wrap_log_lines_500_lines_width_80` | `ui::log_view::wrap_log_lines`, 500 synthetic lines (variable-length messages including timestamps and Japanese text), width 80 | 650.70 µs |
| `archive_listing/zip_100_entries` | `virtual_dir::read_archive_dir_entries`, zip, 100 entries (with nesting) | 274.90 µs |
| `archive_listing/tar_gz_100_entries` | Same as above, tar.gz, 100 entries (with nesting) | 101.67 µs |

For each bench's raw log (including outlier info), see the `cargo bench` standard output. criterion also saves HTML reports and raw data (`estimates.json`, etc.) under `target/criterion/`, so subsequent `cargo bench` runs will automatically compare against it and print "Performance has improved/regressed".

## Results (after Phase 1, median)

Changes made in Phase 1: precomputing `FsEntry::name_lower`/`ext_lower` (eliminating per-call allocations in `compare_entries`/`FilterSpec::matches`/`jump_matches`), a sorted-index cache for `Pane::visible_entries` (`Rc<Vec<usize>>`, explicitly invalidated via `invalidate_visible_cache`), a consolidated `Pane::selected_entry` accessor, and a dirty flag in `main.rs` (`App::needs_redraw`) + a longer idle polling interval (50ms → 250ms). The execution environment is the same machine and profile as in the "Execution environment" section above (rustc 1.94.0, criterion 0.8.2).

`pane_visible_entries/*` uses the same measurement method as Phase 0 (starting over from `Pane::new` every iteration, always the cache-miss path). The newly added `pane_visible_entries_cached/*` creates the `Pane` once outside `b.iter` and measures only the cache-hit path thereafter — this is closer to what actually happens repeatedly in the real app on cursor movement and redraws.

| Bench | Target | Phase 0 median | After Phase 1 median | Change |
|---|---|---|---|---|
| `pane_visible_entries/1000_files/Name` | Cache miss (`Pane::new` every time), 1,000 files, Name | 2.5231 ms | 1.3949 ms | -44.7% |
| `pane_visible_entries/1000_files/Ext` | Same as above, Ext | 3.0284 ms | 1.4024 ms | -54.2% |
| `pane_visible_entries/10000_files/Name` | Cache miss, 10,000 files, Name | 33.305 ms | 18.454 ms | -44.4% |
| `pane_visible_entries/10000_files/Ext` | Same as above, Ext | 40.049 ms | 18.394 ms | -53.9% |
| `pane_visible_entries_cached/1000_files/cache_hit` | Cache hit, 1,000 files (no corresponding bench in Phase 0) | — | 342.87 ns | Reference: about -99.98% (1/4070) vs. the miss path |
| `pane_visible_entries_cached/10000_files/cache_hit` | Cache hit, 10,000 files (no corresponding bench in Phase 0) | — | 3.2360 µs | Reference: about -99.98% (1/5700) vs. the miss path |
| `wrap_log_lines_500_lines_width_80` | Unchanged (control bench for comparison) | 650.70 µs | 654.94 µs | Within noise (+0.4%) |
| `archive_listing/zip_100_entries` | `read_archive_dir_entries` (one extra `FsEntry::name_lower`/`ext_lower` computation per entry) | 274.90 µs | 280.19 µs | +1.8% (nearly noise, as expected) |
| `archive_listing/tar_gz_100_entries` | Same as above | 101.67 µs | 100.58 µs | -1.7% (within noise) |

Observations:

- The cache-miss path (`pane_visible_entries/*`) improved about 44–54% versus Phase 0 thanks to eliminating per-comparison allocations in `compare_entries`/`extension_lower`. Even including the overhead of precomputation (`read_dir_entries` calling `lower_keys` once per entry), it is still a large improvement, with no regression on the miss path.
- The cache-hit path (`pane_visible_entries_cached/*`) is about 4000–5700x faster than the miss path — this is where the real-world impact matters most (cursor movement, per-frame rendering, and the `selected_entry`-family accessors almost always go through the hit path).
- The +1.8% on `archive_listing/zip_100_entries` is the cost of one additional `lower_keys` call added on the `group_children` side — a level that's hard to distinguish from noise (the tar_gz side moved in the opposite direction, -1.7%, offsetting it). This is not on the hot path to begin with, so it's acceptable.
- The dirty flag + extended polling interval (in the `run` loop in main.rs) is outside the scope of criterion benches (it concerns the overall event loop behavior). Its effect should be checked via the "Manual idle-CPU measurement procedure" section of `benches/BASELINE.md` — this round only ran automated tests (`cargo test`) and manual verification via `cargo run --release`; actual idle-CPU numbers have not yet been recorded.

## Results (after Phase 2, median)

Changes made in Phase 2: changed the log bottom panel (`ui::log_view::render_log_lines`) from wrapping all lines and then taking a tail slice, to the newly added `wrap_log_lines_tail` (wraps from the last line backwards and stops as soon as enough rows are produced). Added a pre-formatted timestamp string (`formatted_timestamp`) to `LogLine`, formatted once with `chrono::format` at `App::log_push` time (this benefits both `wrap_log_lines` and `wrap_log_lines_tail` — so not just the latter, but also the former's path, i.e. the full log view/search, gets faster too). Also added a full-log-wrap cache to `App` keyed on `(log_generation, width)`, a help/settings keybinding row cache keyed on `Keymap::generation` (an id assigned only when generated — since after generation it's only ever replaced, never mutated in place), and a function-list filter result cache keyed on the typed query. Changed the settings category list and the function-list palette to build `ListItem`s only for the visible window (avoiding `combos_for`/formatting for off-screen entries). The viewer's `slice_display_cols` now short-circuits to a direct byte-range slice when a line is all ASCII (for horizontal scrolling in huge single-line files). The `/`/`?` search now holds a `Matcher` (compiled regex) via `Rc` in `ViewerSearch::Active`, eliminating recompilation on every redraw across all three of the viewer/help/log screens. Filter input no longer re-runs `FilterSpec::parse` if the raw text hasn't changed. The execution environment is the same machine and profile as in the "Execution environment" section above (rustc 1.94.0, criterion 0.8.2).

`wrap_log_lines_tail_500_lines_width_80_need_4_rows` is the new bench for this round (no corresponding bench in Phase 0/1) — it models the typical case for the log bottom panel (only the last 4 or so of 500 lines are actually needed). The `pane_visible_entries/*` family is code (`pane.rs`) that Phase 2 did not touch at all, but this day's measurements fluctuated by about ±5–11% (re-measuring `pane_visible_entries/1000_files/Name` alone came out -5.4% in the opposite direction) — judged to be noise from machine load and safe to ignore.

| Bench | Target | After Phase 1 median | After Phase 2 median | Change |
|---|---|---|---|---|
| `wrap_log_lines_500_lines_width_80` | The path taken by the full log view/search (wrapping all 500 lines). Directly benefits from the `formatted_timestamp` precomputation | 654.94 µs | 540.72 µs | -17.4% (from removing the chrono format call) |
| `wrap_log_lines_tail_500_lines_width_80_need_4_rows` | The path actually taken by the log bottom panel (only the last 4 lines needed). No corresponding bench in Phase 0/1 | — | 2.5848 µs | Reference: about -99.5% (1/209) vs. the full wrap |
| `pane_visible_entries/1000_files/Name` | Unchanged code (control) | 1.3949 ms | 1.5003 ms (1.4366 ms on re-measurement) | Within noise (this day's run-to-run variance was ±5–11%) |
| `pane_visible_entries/1000_files/Ext` | Unchanged code (control) | 1.4024 ms | 1.4187 ms | Within noise (+1.2%) |
| `pane_visible_entries/10000_files/Name` | Unchanged code (control) | 18.454 ms | 20.319 ms | Within noise (run-to-run variance, discussed below) |
| `pane_visible_entries/10000_files/Ext` | Unchanged code (control) | 18.394 ms | 19.970 ms | Within noise (run-to-run variance, discussed below) |
| `pane_visible_entries_cached/1000_files/cache_hit` | Unchanged code (control) | 342.87 ns | 347.14 ns | Within noise (+1.2%) |
| `pane_visible_entries_cached/10000_files/cache_hit` | Unchanged code (control) | 3.2360 µs | 3.2402 µs | No change |
| `archive_listing/zip_100_entries` | Unchanged code (control) | 280.19 µs | 280.15 µs | No change |
| `archive_listing/tar_gz_100_entries` | Unchanged code (control) | 100.58 µs | 101.92 µs | Within noise (+1.3%) |

Observations:

- The main goal of this phase (reducing the per-frame cost of the log bottom panel) can be confirmed directly with the `wrap_log_lines_tail` bench: about 1/209 (a 99.5% reduction) compared to a full 500-line wrap. Since this path runs every frame in the real app (the whole time it's running), the felt improvement should be significant, on par with the `pane_visible_entries` cache-hit change in Phase 1.
- `wrap_log_lines_500_lines_width_80` (the full log view/search path, whose code itself is still "wrap all lines" and unchanged) also improved by -17.4% — this comes from precomputing `LogLine::formatted_timestamp` (`chrono::format` called once at `App::log_push` time, with only a string clone from then on in both `wrap_log_lines` and `wrap_log_lines_tail`), an effect separate from the tail-first change itself.
- The keybinding row caches for help/settings, the function-list filter result cache, `Matcher` reuse across viewer/help/log, and suppressing filter input re-parsing are all changes in the category of "don't recompute the same frame over and over" — these are hard to measure with criterion's single-call benches (the benefit is in how often they're called, not the per-call cost, which is essentially unchanged). Correctness is backed by `cargo test` (including new tests verifying the caches actually track keymap/query changes); no interactive manual confirmation was done this round.
- `pane_visible_entries/*`, `pane_visible_entries_cached/*`, and `archive_listing/*` are the control group untouched by Phase 2. The +8–10% on `pane_visible_entries/10000_files/*` vs. Phase 1 is judged to be run-to-run noise on this day (other process load, etc.) rather than a code change, since re-measuring `pane_visible_entries/1000_files/Name` alone swung the opposite way, -5.4%.

## Manual idle-CPU measurement procedure

Not an automated bench — a procedure for hands-on, felt confirmation on real hardware.

1. Prepare a release build.

   ```sh
   cargo build --release
   ```

2. Launch the release binary and leave it idle without any operation.

   ```sh
   ./target/release/ozzel
   ```

3. Open another terminal and check the PID of the running ozzel.

   ```sh
   pgrep -x ozzel
   ```

4. Sample `%cpu` for that PID with `ps` a few times (e.g. 5–10 times, at 1–2 second intervals) and average them.

   ```sh
   for i in $(seq 1 10); do ps -o %cpu= -p <pid>; sleep 1; done
   ```

   As a one-liner that also computes the average:

   ```sh
   for i in $(seq 1 10); do ps -o %cpu= -p <pid>; sleep 1; done | awk '{s+=$1; n++} END {print s/n "%"}'
   ```

5. Append the resulting average to this file and compare before/after each refactoring round. Since the event loop runs on a `Duration::from_millis(50)` polling interval (the `run` function in `main.rs`), some CPU usage proportional to this polling cycle occurs even while idle — this would be the target if there's room to reduce it.

## Results (after Phase 3, median)

Changes made in Phase 3:

1. **`VirtualDir` entry listing cache** (`src/virtual_dir.rs`): keeps the archive's raw entry listing (all `RawEntry`s) in `VirtualDir` (`Rc<RefCell<Option<CachedEntries>>>`), eliminating re-opening/re-parsing/re-decompression on every descend/go_parent/`Pane::reload` as long as `(mtime, len)` hasn't changed. This avoids re-parsing the central directory for zip, re-scanning the stream for tar formats, and re-inflating the entire archive for `.tar.xz`. `VirtualDir::clone` (used by `Pane::virtual_go_parent`) shares the cache (just clones the `Rc`), so the same cache keeps being used whether descending into or ascending out of the archive. If the archive file itself has been replaced (mtime/len changed), it is reloaded. If the `stat` call itself fails (e.g. after the archive was deleted), the existing cache is used as-is (a stat failure for listing purposes is treated as "no change").
2. **Faster copying** (`src/tasks/copy_move.rs`): files at or below `WHOLE_FILE_COPY_THRESHOLD` (4 MiB) now go through a single `std::fs::copy` call (which benefits from the OS's clonefile/copy_file_range, and also copies permissions). Files above that threshold still use the previous chunked read/write approach (reusing a single 1 MiB buffer allocated once in `TransferCtx::buf` across all files, keeping the per-chunk cancellation check and progress reporting), and after completion explicitly copies `src`'s permissions to `dest` to make both paths behave the same way.
3. **Initial draw at startup** (`src/main.rs`/`src/app.rs`): changed the order to construct both panes empty via `App::new_unloaded` → run `terminal.draw` once at the top of `run` → load the actual directories via `App::load_initial_dirs` → the normal loop. This means the screen appears right after entering the alternate screen, even with slow mounts or huge directories. The external behavior of `App::new` itself (the path used by existing tests / `test_app`) is unchanged (it remains the combination of `new_unloaded` + `load_initial_dirs`, i.e. eager loading is preserved).

`archive_listing_cached/*` is the new bench for this round (no corresponding bench in Phase 0–2) — it measures the path taken by repeated calls on the same instance after warming the cache with one call to `VirtualDir::list`, i.e. the path actually taken every time you descend/go_parent within the app. `archive_listing/*` (calling `read_archive_dir_entries` directly, always a cold path that bypasses `VirtualDir`) remains as the control group untouched in Phase 3. The execution environment is the same machine and profile as in the "Execution environment" section above (rustc 1.94.0, criterion 0.8.2).

| Bench | Target | After Phase 2 median | After Phase 3 median | Change |
|---|---|---|---|---|
| `archive_listing/zip_100_entries` | Always cold (`read_archive_dir_entries` called directly, control) | 280.15 µs | 273.69 µs | Within noise (-2.3%) |
| `archive_listing/tar_gz_100_entries` | Same as above (control) | 101.92 µs | 98.875 µs | Within noise (-3.0%) |
| `archive_listing/tar_xz_100_entries` | Same as above (control, no corresponding bench in Phase 2) | — | 93.515 µs | — |
| `archive_listing_cached/zip_100_entries/cache_hit` | `VirtualDir::list` cache hit (no corresponding bench in Phase 2) | — | 12.891 µs | Reference: about -95.3% (1/21) vs. cold |
| `archive_listing_cached/tar_gz_100_entries/cache_hit` | Same as above | — | 12.866 µs | Reference: about -87.0% (1/7.7) vs. cold |
| `archive_listing_cached/tar_xz_100_entries/cache_hit` | Same as above | — | 12.849 µs | Reference: about -86.3% (1/7.3) vs. cold |
| `pane_visible_entries/1000_files/Name` | Unchanged code (control) | 1.5003 ms | 1.2954 ms | Within noise (-13.6%, run-to-run variance) |
| `pane_visible_entries/10000_files/Name` | Unchanged code (control) | 20.319 ms | 15.774 ms | Within noise (-22.4%, run-to-run variance) |
| `pane_visible_entries_cached/10000_files/cache_hit` | Unchanged code (control) | 3.2402 µs | 3.2054 µs | No change |
| `wrap_log_lines_500_lines_width_80` | Unchanged code (control) | 540.72 µs | 521.85 µs | Within noise (-3.5%) |
| `wrap_log_lines_tail_500_lines_width_80_need_4_rows` | Unchanged code (control) | 2.5848 µs | 2.5264 µs | Within noise (-2.3%) |

Observations:

- `archive_listing_cached/*` comes in at about 12.8–12.9 µs across all three formats — once the cache is hit, the cost of `group_children` (an in-memory `HashMap` filter) dominates, and the original format differences (zip's central directory re-parse vs. tar's stream re-scan vs. xz's full expansion) disappear entirely. This is as intended: the per-descend/go_parent/`reload` cost has collapsed to a small, constant cost regardless of format.
- Compared to the cold path (`archive_listing/*`), the reduction is largest for zip (about 1/21), and about 1/7–1/8 for tar_gz/tar_xz. zip's central directory re-parse cost was already heavier than tar's sequential stream scan to begin with, so the caching benefit shows up directly in the ratio.
- For `.tar.xz`, at this bench's archive size (100 entries, 17-byte payload each), the cost of full expansion is still small, so the reduction ratio isn't much different from tar_gz. For a huge archive of the kind the plan flagged as problematic ("re-decompressing 2GB of tar.xz just to descend one level"), the gap with/without caching would open up by orders of magnitude in absolute time (full-expansion cost scales with archive size, whereas post-cache-hit cost depends only on the entry count).
- `pane_visible_entries/*` and `wrap_log_lines*` are the control group untouched by Phase 3. This run's numbers moved uniformly in the improved direction compared to the previous measurement, but since this is code with no changes, it's judged — as in the Phase 1/2 records — to be run-to-run noise (machine load differences) on this day, and not counted as an effect of Phase 3. `pane_visible_entries_cached/10000_files/cache_hit` is the reference point least affected by noise, useful for confirming "effectively no change."
- The copy speedup (`src/tasks/copy_move.rs`) and the initial draw at startup (`main.rs`/`app.rs`) are the kind of change that's hard to measure with criterion's single-function-call benches (the former depends on the OS's own optimizations for `std::fs::copy`, the latter concerns the overall look of the event loop's behavior). Correctness is backed by new `cargo test` tests (`copy_preserves_source_permissions_on_both_the_small_and_chunked_paths`, `run_copy_of_a_large_file_goes_through_the_chunked_path_and_finishes_ok`, `new_unloaded_builds_both_panes_with_no_entries_and_no_io`, etc.). Felt confirmation was manual-only, via `cargo run --release` (reduced allocations when copying huge files, empty panes appearing immediately at startup).

### Design decision notes

- **On keeping the decompressed xz buffer**: repeated re-decompression for listing purposes alone has been resolved by the cache, but the path for extracting/viewing a single file (`extract_single_from_tar`, and for `.tar.xz`, the full expansion in `open_tar_archive`) still re-decompresses every time, unchanged, within this phase's scope. Reasons: (1) keeping the decompressed buffer in `VirtualDir` would mean that merely browsing a huge `.tar.xz` (the 2GB-class case mentioned in the plan) would keep the whole thing resident in memory, which is a disproportionately large memory cost relative to the goal of "avoid re-decompression for listing." (2) extracting a single file only happens once per user action (opening in the viewer / extracting), not "every time you change directories" like listing — so the cost reduction per occurrence is much smaller than for the listing cache. Given the above, "cache listing, keep single extraction as-is" is judged to meet this phase's goal (eliminating heavy re-decoding on every browsing operation), and caching the single-extraction side has been deferred as future work.
- **On the copy threshold (`WHOLE_FILE_COPY_THRESHOLD` = 4 MiB)**: files that finish copying in less time than the progress bar's update interval (`PROGRESS_MIN_INTERVAL` = 100ms) barely get a chance for chunked progress reporting to show up on screen anyway. 4 MiB was chosen as a rough estimate of "a size that a typical disk/SSD can transfer within 100ms." At or below this threshold, a single `std::fs::copy` call is used (cancellation is only checked before the file starts); above it, the previous chunked loop is used as before (cancellation checked per chunk, progress reporting continues). This resolves the issue where copying a large number of small files (e.g. 100,000 files) caused a zeroed `vec![0u8; 1MiB]` allocation per file, while leaving unchanged the cancellation responsiveness and progress display felt during the copy of a single large file.
