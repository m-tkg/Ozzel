//! Phase 0 baseline benches — see `benches/BASELINE.md` for the recorded
//! numbers and how to re-run/compare them. Targets exercised through
//! `ozzel`'s public (or bench-only-`pub`, see the doc comments at each
//! call site) API rather than reimplementing setup logic here.

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use chrono::{Local, TimeZone};
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use ozzel::app::LogLine;
use ozzel::pane::{Pane, SortKey};
use ozzel::ui::log_view::wrap_log_lines;
use ozzel::virtual_dir::read_archive_dir_entries;

// --- Pane::visible_entries (directory read + sort) -------------------------

fn make_dir_with_files(count: usize) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..count {
        // Alternating extensions so the `Ext` sort key has more than one
        // bucket to actually sort across, instead of a no-op tie-break.
        let ext = if i % 2 == 0 { "txt" } else { "rs" };
        let path = dir.path().join(format!("file_{i:06}.{ext}"));
        fs::write(&path, b"x").unwrap();
    }
    dir
}

fn bench_pane_visible_entries(c: &mut Criterion) {
    let mut group = c.benchmark_group("pane_visible_entries");
    group.sample_size(20);

    for &count in &[1_000usize, 10_000usize] {
        let dir = make_dir_with_files(count);

        for sort in [SortKey::Name, SortKey::Ext] {
            let id = BenchmarkId::new(format!("{count}_files"), format!("{sort:?}"));
            group.bench_with_input(id, &sort, |b, &sort| {
                b.iter(|| {
                    let mut pane = Pane::new(dir.path().to_path_buf()).unwrap();
                    pane.sort = sort;
                    std::hint::black_box(pane.visible_entries().len())
                });
            });
        }
    }
    group.finish();
}

/// Phase 1's cache-hit path: the `Pane` (and its `visible_entries` cache)
/// is built once *outside* `b.iter`, so every timed iteration is a pure
/// cache hit — an `Rc` refcount bump plus rebuilding the small `Vec<VisibleItem>`
/// wrapper, never a re-filter/re-sort. Companion to
/// `bench_pane_visible_entries` above, which stays a cold-cache (`Pane::new`
/// every iteration) measurement on purpose so the miss path's own cost
/// (now alloc-free per comparison, see `FsEntry::name_lower`/`ext_lower`)
/// stays visible too.
fn bench_pane_visible_entries_cached(c: &mut Criterion) {
    let mut group = c.benchmark_group("pane_visible_entries_cached");
    group.sample_size(20);

    for &count in &[1_000usize, 10_000usize] {
        let dir = make_dir_with_files(count);
        let pane = Pane::new(dir.path().to_path_buf()).unwrap();

        let id = BenchmarkId::new(format!("{count}_files"), "cache_hit");
        group.bench_with_input(id, &(), |b, ()| {
            b.iter(|| std::hint::black_box(pane.visible_entries().len()));
        });
    }
    group.finish();
}

// --- ui::log_view::wrap_log_lines ------------------------------------------

fn make_log_lines(count: usize) -> Vec<LogLine> {
    let base = Local.timestamp_opt(1_700_000_000, 0).unwrap();
    (0..count)
        .map(|i| {
            // Mixed-length, partly Japanese messages so wrapping actually
            // has varied line counts to chew through, not a uniform case.
            let message = if i % 3 == 0 {
                format!("ファイル操作 {i}: /some/長い/日本語パス/を含む/ログ行/です — 完了しました")
            } else if i % 3 == 1 {
                format!("copied item {i} to /tmp/destination/path/that/is/reasonably/long")
            } else {
                format!("error {i}: operation failed")
            };
            LogLine {
                message,
                is_error: i % 3 == 2,
                timestamp: base + chrono::Duration::seconds(i as i64),
            }
        })
        .collect()
}

fn bench_wrap_log_lines(c: &mut Criterion) {
    let lines = make_log_lines(500);
    c.bench_function("wrap_log_lines_500_lines_width_80", |b| {
        b.iter(|| std::hint::black_box(wrap_log_lines(lines.iter(), 80)));
    });
}

// --- virtual_dir archive listing -------------------------------------------

fn nested_entry_path(i: usize) -> String {
    // A handful of entries land a couple of directory levels deep so
    // listing exercises `group_children`'s intermediate-directory
    // synthesis, not just a flat root listing.
    match i % 5 {
        0 => format!("dir_a/nested/entry_{i:04}.txt"),
        1 => format!("dir_b/entry_{i:04}.txt"),
        _ => format!("entry_{i:04}.txt"),
    }
}

fn make_zip_archive(entry_count: usize) -> (tempfile::TempDir, PathBuf) {
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    let dir = tempfile::tempdir().unwrap();
    let archive_path = dir.path().join("bench.zip");
    let file = fs::File::create(&archive_path).unwrap();
    let mut writer = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for i in 0..entry_count {
        writer.start_file(nested_entry_path(i), options).unwrap();
        writer.write_all(b"benchmark payload").unwrap();
    }
    writer.finish().unwrap();
    (dir, archive_path)
}

fn make_tar_gz_archive(entry_count: usize) -> (tempfile::TempDir, PathBuf) {
    let mut builder = tar::Builder::new(Vec::new());
    for i in 0..entry_count {
        let data = b"benchmark payload";
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, nested_entry_path(i), &data[..])
            .unwrap();
    }
    let tar_bytes = builder.into_inner().unwrap();

    let dir = tempfile::tempdir().unwrap();
    let archive_path = dir.path().join("bench.tar.gz");
    let file = fs::File::create(&archive_path).unwrap();
    let mut encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    encoder.write_all(&tar_bytes).unwrap();
    encoder.finish().unwrap();
    (dir, archive_path)
}

fn bench_archive_listing(c: &mut Criterion) {
    let mut group = c.benchmark_group("archive_listing");
    group.sample_size(20);

    let (_zip_dir, zip_path) = make_zip_archive(100);
    group.bench_function("zip_100_entries", |b| {
        b.iter(|| {
            std::hint::black_box(
                read_archive_dir_entries(&zip_path, std::path::Path::new("")).unwrap(),
            )
        });
    });

    let (_tar_dir, tar_gz_path) = make_tar_gz_archive(100);
    group.bench_function("tar_gz_100_entries", |b| {
        b.iter(|| {
            std::hint::black_box(
                read_archive_dir_entries(&tar_gz_path, std::path::Path::new("")).unwrap(),
            )
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_pane_visible_entries,
    bench_pane_visible_entries_cached,
    bench_wrap_log_lines,
    bench_archive_listing
);
criterion_main!(benches);
