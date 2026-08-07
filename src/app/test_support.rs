//! Shared test-only helpers for `app`'s test suite (`src/app/tests/`).

use super::*;
use std::path::Path;
use std::time::Duration;

pub(super) fn test_app(left: &Path, right: &Path) -> App {
    App::new(left.to_path_buf(), right.to_path_buf(), Config::default()).unwrap()
}

/// Drains tasks in a loop until none are running, or panics after a
/// generous timeout. Background tasks in these tests are tiny
/// (single small files in a tempdir), so this should resolve almost
/// immediately; the loop only exists to avoid a fixed sleep racing the
/// worker thread.
pub(super) fn wait_for_tasks_done(app: &mut App) {
    for _ in 0..500 {
        app.drain_tasks();
        if app.tasks.running.is_empty() {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("background task(s) did not finish in time");
}

pub(super) fn select_entry_named(app: &mut App, name: &str) {
    let idx = app
        .active_pane()
        .visible_entries()
        .iter()
        .position(|item| matches!(item, crate::pane::VisibleItem::Entry(e) if e.name == name))
        .unwrap();
    app.active_pane_mut().cursor = idx;
}

/// Reads back whichever real entry the pane's cursor currently sits
/// on, panicking if it's on `..` or out of bounds — used by the
/// post-delete cursor-anchor tests below to assert exactly where the
/// cursor landed.
pub(super) fn cursor_entry_name(app: &App) -> String {
    match app
        .active_pane()
        .visible_entries()
        .get(app.active_pane().cursor)
    {
        Some(crate::pane::VisibleItem::Entry(e)) => e.name.clone(),
        other => panic!("expected cursor on a real entry, got {other:?}"),
    }
}

pub(super) fn scroll_of(app: &App) -> usize {
    match &app.mode {
        Mode::Viewer { scroll, .. } => *scroll,
        other => panic!("expected Mode::Viewer, got {other:?}"),
    }
}

pub(super) fn h_scroll_of(app: &App) -> usize {
    match &app.mode {
        Mode::Viewer { h_scroll, .. } => *h_scroll,
        other => panic!("expected Mode::Viewer, got {other:?}"),
    }
}

pub(super) fn view_mode_of(app: &App) -> ViewMode {
    match &app.mode {
        Mode::Viewer { view_mode, .. } => *view_mode,
        other => panic!("expected Mode::Viewer, got {other:?}"),
    }
}

pub(super) fn search_of(app: &App) -> ViewerSearch {
    match &app.mode {
        Mode::Viewer { search, .. } => search.clone(),
        other => panic!("expected Mode::Viewer, got {other:?}"),
    }
}

pub(super) fn type_into_viewer(app: &mut App, s: &str) {
    for c in s.chars() {
        app.handle_event(AppEvent::Input(KeyCode::Char(c), KeyModifiers::NONE));
    }
}

/// A file with a distinct, greppable line at indices 2, 5, and 8 (and
/// filler lines everywhere else) — enough spread to exercise wraparound
/// in both directions without the matches being adjacent.
pub(super) fn write_needle_file(dir: &Path) -> PathBuf {
    let mut lines = vec!["filler".to_string(); 12];
    lines[2] = "the needle is here".to_string();
    lines[5] = "another needle spot".to_string();
    lines[8] = "final needle line".to_string();
    let path = dir.join("haystack.txt");
    std::fs::write(&path, lines.join("\n")).unwrap();
    path
}

pub(super) fn open_viewer_on(app: &mut App, name: &str) {
    app.active_pane_mut().reload().unwrap();
    select_entry_named(app, name);
    app.dispatch(Action::Open);
}

pub(super) fn help_scroll_of(app: &App) -> usize {
    match &app.mode {
        Mode::Help { scroll, .. } => *scroll,
        other => panic!("expected Mode::Help, got {other:?}"),
    }
}

pub(super) fn test_layout(
    area: ratatui::layout::Rect,
    header_rows: u16,
    start: usize,
) -> PaneLayout {
    PaneLayout {
        area,
        rows_area: ratatui::layout::Rect {
            x: area.x + 1,
            y: area.y + 1 + header_rows,
            width: area.width.saturating_sub(2),
            height: area.height.saturating_sub(2 + header_rows),
        },
        start,
    }
}

pub(super) fn left_pane_area() -> ratatui::layout::Rect {
    ratatui::layout::Rect {
        x: 0,
        y: 0,
        width: 20,
        height: 10,
    }
}

pub(super) fn right_pane_area() -> ratatui::layout::Rect {
    ratatui::layout::Rect {
        x: 20,
        y: 0,
        width: 20,
        height: 10,
    }
}

/// Builds a test `App` with two populated dirs (`sub` and files inside
/// `left`) and both panes' `pane_layout` filled in, mirroring what
/// `ui::draw` sets up every real frame — mouse tests need this to
/// resolve screen coordinates back to entries.
pub(super) fn mouse_test_app() -> (tempfile::TempDir, App) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"hi").unwrap();
    std::fs::write(dir.path().join("b.txt"), b"hi").unwrap();
    std::fs::create_dir(dir.path().join("c_dir")).unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.pane_layout = [
        Some(test_layout(left_pane_area(), 0, 0)),
        Some(test_layout(right_pane_area(), 0, 0)),
    ];
    (dir, app)
}

pub(super) fn click(app: &mut App, column: u16, row: u16) {
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    });
}

pub(super) fn release(app: &mut App, column: u16, row: u16) {
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    });
}

pub(super) fn drag(app: &mut App, column: u16, row: u16) {
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    });
}

pub(super) fn make_test_archive(dir: &Path) -> PathBuf {
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    let archive_path = dir.join("project.zip");
    let file = std::fs::File::create(&archive_path).unwrap();
    let mut writer = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    writer.start_file("readme.txt", options).unwrap();
    writer.write_all(b"hello from inside the zip").unwrap();
    writer.start_file("src/main.rs", options).unwrap();
    writer.write_all(b"fn main() {}").unwrap();
    writer.finish().unwrap();
    archive_path
}

/// A `.tar.gz` with the same layout `make_test_archive` gives its zip —
/// the non-zip counterpart the extension-aware `u` needs. Named
/// `project.tar.gz` specifically so a test can tell `archive_stem`
/// (`project`) apart from `Path::file_stem` (`project.tar`).
pub(super) fn make_test_tar_gz(dir: &Path) -> PathBuf {
    use std::io::Write;

    let mut builder = tar::Builder::new(Vec::new());
    for (name, body) in [
        ("readme.txt", &b"hello from inside the tar"[..]),
        ("src/main.rs", &b"fn main() {}"[..]),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, name, body).unwrap();
    }
    let tar_bytes = builder.into_inner().unwrap();

    let archive_path = dir.join("project.tar.gz");
    let mut encoder = flate2::write::GzEncoder::new(
        std::fs::File::create(&archive_path).unwrap(),
        flate2::Compression::default(),
    );
    encoder.write_all(&tar_bytes).unwrap();
    encoder.finish().unwrap();
    archive_path
}

/// A bare `.gz` holding a single `notes.txt` payload — an
/// `ArchiveKind::Single` archive, which `u` extracts without a wrapper
/// directory.
pub(super) fn make_test_gz(dir: &Path, payload: &[u8]) -> PathBuf {
    use std::io::Write;

    let archive_path = dir.join("notes.txt.gz");
    let mut encoder = flate2::write::GzEncoder::new(
        std::fs::File::create(&archive_path).unwrap(),
        flate2::Compression::default(),
    );
    encoder.write_all(payload).unwrap();
    encoder.finish().unwrap();
    archive_path
}

/// Moves the cursor onto the visible entry named `name`, panicking if
/// it isn't currently visible — used throughout these tests instead of
/// hardcoding row indices, which would silently break if sort order
/// or the parent-row rule ever changes.
pub(super) fn move_cursor_onto(pane: &mut Pane, name: &str) {
    let idx = pane
        .visible_entries()
        .iter()
        .position(|item| matches!(item, crate::pane::VisibleItem::Entry(e) if e.name == name))
        .unwrap_or_else(|| panic!("{name} not visible"));
    pane.cursor = idx;
}

/// Points `app` at a fresh, empty config file in `dir` and returns its
/// path — every settings test needs this, since `settings_save` must
/// never touch a real user's config while `cargo test` runs.
pub(super) fn settings_test_app(dir: &Path) -> (App, PathBuf) {
    let left = tempfile::tempdir().unwrap();
    let mut app = test_app(left.path(), left.path());
    let config_path = dir.join("config.toml");
    std::fs::write(&config_path, "").unwrap();
    app.settings_config_path = Some(config_path.clone());
    (app, config_path)
}
