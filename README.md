# ozzel

`ozzel` is a TUI (text user interface) file manager written in Rust. It displays two directories side by side in left and right panes, and lets you copy, move, delete, zip, and unzip files with single-key commands. It runs cross-platform on macOS, Linux, and Windows.

![The two-pane view: git status markers and a branch tag on the left pane, marked files in yellow, the cursor row highlighted, and the log pane below](docs/images/main-view.png)

## Overview

- Two-pane directory browser (`Tab` to switch panes, `Enter` to open, `Backspace` to go to the parent directory)
- Single-key commands for copy, move, delete, rename, mkdir, sort toggling, hidden-file visibility toggling, and more
- Copy and move (like delete) show a confirmation dialog before executing (can be disabled in settings; overwrite conflicts are always confirmed regardless)
- Full key binding remapping via a TOML config file (both `[keys]`: key → action, and `[bindings]`: action → array of keys, are supported). The example config has all default key bindings written out under `[bindings]`, ready to edit directly
- A help screen showing the currently effective key bindings (`h` / `?`)
- A shortcut to edit the config file in place and apply changes immediately (`,`; creates a new file from a template if none exists yet, and reloads the config without restarting after saving)
- Marking multiple files (`Space`) and batch operations on marked files
- Incremental filtering/search (substring match, or regex with the `re:` prefix)
- Prefix jump (`\`) — an incremental search that moves only the cursor by prefix match, without filtering the list. `Down`/`Up` cycles to the next/previous match, wrapping around at the ends
- Copy, move, delete, zip compression/extraction, and extraction from a Virtual Directory all run asynchronously on background threads, showing a progress bar while other operations continue
- Each line in the log pane shows its recorded timestamp (long messages wrap, and continuation lines are indented by the width of the timestamp so columns line up). Press `L` to open a full-screen log viewer showing the entire log
- Zip compression/extraction (protected against zip-slip)
- A Virtual Directory feature that lets you browse, navigate, and partially extract archives (`.zip`, `.tar`, `.tar.gz`/`.tgz`, `.tar.bz2`/`.tbz2`, `.tar.xz`/`.txz`, plus bare `.gz`/`.bz2` as one-entry archives) as if they were directories, without extracting them. Password-protected zips work too (a masked prompt appears the first time contents are read; remembered for the session)
- External viewer commands configurable per file extension (`[viewers]`). Extensions with no configured entry fall back to the built-in viewer
- Persistent directory history and bookmarks, plus a per-pane temporary back/forward history (`Shift+←`/`Shift+→`)
- A built-in viewer (open files with `Enter`/`o`; supports displaying Japanese text; `Tab` toggles between text display and an `xxd`-style hex dump). Supports `less`-compatible scrolling (`j`/`k`/`d`/`u`/`f`/`b`/`Space`) and search (`/`, `?`, `n`, `N`, with regex preferred and substring-match fallback, plus match highlighting)
- Launching external commands with `:` and an editor with `e` (both temporarily suspend the TUI)
- Command palette (`F`) — incrementally filter and run any action by name
- A raspi-config-style settings screen (`S`) — a three-level, full-screen category → item → edit UI that lets you change behavior, colors, startup integration, extension viewers, and key bindings (add/remove, with conflict detection) without editing the config file directly. Edits are saved and applied immediately, and the layout of the existing config file, including comments, is preserved
- Cursor color (both active and inactive), and the row colors for directories, hidden files, and executable files, are all customizable in settings (named color or `#RRGGBB`)
- The inactive pane is dimmed (can be disabled in settings)
- A permissions column (unix: `drwxr-xr-x` format, Windows: simplified display) shown in the entry list (can be disabled in settings; automatically hidden when the pane is too narrow)
- Copy a file/directory's absolute path to the clipboard (`y`), and duplicate the entry under the cursor within the same directory (`c`)
- Mouse support (click to focus/move the cursor, wheel scrolling, drag to toggle-mark a range, double-click to open; can be disabled in settings)
- A mechanism to write the focused pane's directory to a file on exit, letting the shell's `cd` follow along (`--cwd-file`)
- Rendering that correctly computes the display width of full-width characters, including Japanese filenames
- `ozzel update` updates itself to the latest version on GitHub (runs `cargo install --git` internally; requires `cargo`; only works once the repository is public)
- Symbolic links pointing to directories are treated the same as directories for navigation and display (`Enter` descends, shown as `<DIR>`, directory color), while copy, move, delete, duplicate, and zip act only on the link itself and never follow it — an intentionally asymmetric design (details below)

## Installation

Requires the Rust toolchain (1.85 or later; Rust 2024 edition is required).

```sh
git clone https://github.com/m-tkg/Ozzel.git
cd Ozzel
cargo build --release
```

The built binary is produced at `target/release/ozzel`. You can also install it to a location on your `PATH` with `cargo install --path .`.

You can also install directly without cloning the repository locally (assuming the GitHub repository has been made public).

```sh
cargo install --git https://github.com/m-tkg/Ozzel
```

This fetches, builds, and installs the latest version from the `main` branch. If you already have it installed and just want to update, `ozzel update` (described below) is simpler.

## Launching

```sh
ozzel [left-pane directory] [right-pane directory]
```

If the arguments are omitted, the current directory becomes the initial directory for both panes. If a specified directory does not exist, an error is shown and the program exits.

To have the shell's `cd` follow along on exit, pass the `--cwd-file <path>` flag (for details, see "Making the Shell Follow `cd` on Exit" below).

## `update` Command

```sh
ozzel update          # update if not already the latest version
ozzel update --force  # force reinstall even if the version is the same
```

Updates an installed `ozzel` to the latest version on the `main` branch on GitHub. When run, it displays the current version, fetches the remote version from `Cargo.toml` on GitHub, and compares them; if the version is the same and `--force` was not given, it prints "already up to date" and exits without doing anything. Otherwise (a newer version is available, the remote version check failed, or `--force` was given), it runs `cargo install --git https://github.com/m-tkg/Ozzel --force` to reinstall. The build takes one to two minutes.

This feature requires that **the ozzel repository has been published on GitHub**. (Before it is published, both checking the remote version and reinstalling fail, and a corresponding error message is shown.) Reinstalling also requires the `cargo` command (if it is not installed, a corresponding error is shown).

`update` is parsed as a subcommand with priority over the first positional argument. So if your current directory actually has a directory named `update` that you want to open as the left pane, specify an explicit path such as `ozzel ./update` (or an absolute path).

## Key Reference

These are the default key bindings. They can be freely redefined in the `[keys]` section of the config file (described below).

### Navigation & Display

| Key | Action |
| --- | --- |
| `↑` / `↓` (also `i` / `k`) | Move the cursor |
| `←` / `→` (also `j` / `l`) | Move focus directly to the left/right pane (does nothing if that pane is already active) |
| `PageUp` / `PageDown` | Move the cursor by page |
| `Home` / `End` | Move to the top/bottom of the list |
| `Shift+↑` / `Shift+↓` | Move to the top/bottom of the list (same as `Home`/`End`) |
| `Tab` | Toggle the active pane (unlike `←`/`→`, each press toggles to the opposite side) |
| `Enter` / `o` | If a directory, open it (including `..`); if a file, open it in the built-in viewer (both are unified into a single `open` action, described below) |
| `Backspace` | Go to the parent directory |
| `w` | Swap the left and right panes |
| `s` | Cycle the sort key (name → size → modified time → extension → name…) |
| `t` | Open the sort dialog: choose the sort key **and** ascending/descending in one modal (`↑`/`↓` to move, `Enter` to apply, `Esc` to cancel). The chosen state is also remembered per directory (see below) |
| `v` | Cycle the size column format: bytes → bytes with thousands separators → human (`1.5M`). The choice is written back to the config file, so it persists across restarts (also editable from the settings screen) |
| `z` | Compute the recursive size of the marked directories (or the directory under the cursor) on a background task. Each directory's `<DIR>` cell is replaced with its real size as results arrive (kept only while the pane stays in this directory), size-sorting picks the numbers up, and the log shows a grand total |
| `.` | Toggle showing hidden (dot) files |
| `Ctrl+R` | Reload both panes |
| `Shift+←` | Go back to this pane's previous directory |
| `Shift+→` | Go forward by however much `Shift+←` went back |

`i`/`k`/`j`/`l` are additional cursor-movement/pane-focus keys usable alongside the arrow keys (the arrow keys themselves still work as-is). Since lowercase `k` is already assigned to cursor movement, mkdir is bound only to uppercase `K` (Shift+k).

A pane's header is a fixed two-row area (DYNA-style — the entry list below never shifts as state changes). Row 1 shows the full current path plus the `[flt:]`/`[s:]` tags; a path too long for the pane is shortened in the middle with `…` (`/Users/me/…/project` — the tail, the part you're actually in, keeps the larger share; grapheme/display-width safe). Row 2 shows the git branch (`⎇ main`, green — empty outside a work tree) on the left and the filesystem's free space on the right (`2.79 GB Free`, refreshed on every reload/directory change; while browsing an archive it keeps reporting the real directory holding the archive, which is also where an extraction would land).

`Shift+←`/`Shift+→` form a temporary per-pane history (a back/forward stack). Each time you navigate to a different directory, the previous location is pushed onto the "back" stack, and the "forward" stack (for `Shift+→`) is cleared whenever you move to a new location. If there is nowhere to go back/forward to, nothing happens and a note is logged. The `H` (Shift+h) history menu described later is a separate, persistent, cross-pane list of recently visited locations.

**Natural (digit-as-number) sort:** By default, name sorting compares digit runs as numbers, so `file2.txt` sorts before `file10.txt`. Set `natural_sort = false` in the config (or toggle it in the settings screen) for strict lexicographic order.

**Per-directory sort memory:** Every explicit sort change (`s`'s cycle or the `t` dialog) is remembered for the current directory (up to 200 directories, persisted as `sort_prefs.json` alongside the history file). Revisiting a directory — by any route: `Enter`, `Backspace`, bookmarks, the history menu, `Shift+←`/`Shift+→`, or at startup — restores its remembered key and direction. A directory with no remembered choice keeps whatever sort the pane already had (the pre-existing behavior). When a pane's sort deviates from the default (name, ascending), the pane header shows a tag like `[s:size↓]`.

**Cursor wrap-around:** With `cursor_wrap = true` (default `false`), single-step cursor movement wraps: one step past the last row lands on the first and vice versa. Page movement, `Home`/`End`, and the mouse wheel always stop at the edges regardless.

### Marking & File Operations

| Key | Action |
| --- | --- |
| `Space` | Toggle marking the entry under the cursor, and move the cursor down one |
| `a` | Invert the mark state of all visible entries |
| `C` (Shift+c) | Copy (all marked entries if any exist, otherwise just the entry under the cursor, to the other pane). Confirmed (see below) |
| `M` (Shift+m) | Move (targets determined the same way as `C`). Confirmed (see below) |
| `D` / `d` | Delete (confirmed; moves to trash or deletes permanently depending on settings) |
| `R` / `r` | Rename (prompt pre-filled with the current name) |
| `m` | Rename marked entries one after another (batch rename): each visible marked entry gets its own prompt in display order, titled `Rename (2/5)`. Confirming an unchanged (or empty) name skips that entry; a failed rename is logged and the sequence continues; `Esc` cancels the rest (already-confirmed renames stand). Marks hidden by an active filter are excluded (announced in the log). No cursor fallback — with nothing marked it just logs an error |
| `K` (Shift+k) | Create a new directory (mkdir) |
| `c` | Duplicate the file/directory under the cursor **within the same directory** (prompt pre-filled with the current name; specify the new name. Directories are copied recursively as an async task) |
| `y` | Copy the absolute path of the entry under the cursor to the system clipboard (described below) |
| `Ctrl+K` | Cancel all running background tasks at once (copy, move, delete, zip compression, unzip, Virtual Directory extraction) |
| `@` | Create symbolic links to the marked (or cursor) entries in the other pane's directory, each pointing at the source's **absolute** path. Confirmed like copy/move (`confirm_operations`); an existing destination name is never overwritten (logged as an error instead). Rejected when both panes show the same directory |
| `A` (Shift+a) | Open the permissions (chmod) dialog for the marked (or cursor) entries: a 3×3 `rwx` toggle grid (rows = user/group/other). Arrow keys move over the grid, `Space` toggles the highlighted bit, `0`-`7` set the highlighted row to that octal digit, `Enter` applies the shown absolute mode to every target, `Esc` cancels. setuid/setgid/sticky bits are preserved as-is. Unix only — on Windows it just logs that it isn't supported |
| `T` (Shift+t) | Touch: prompt for a timestamp (pre-filled with the cursor entry's mtime; formats `YYYY-MM-DD HH:MM:SS`, `YYYY-MM-DD HH:MM`, or `YYYY-MM-DD`; empty input = now) and set the marked (or cursor) entries' modified/accessed times to it |
| `I` (Shift+i) | Show a file-information dialog for the entry under the cursor: full path, type, link target (symlinks), exact byte size, permissions + octal mode, owner/group, hard-link count, inode, and modified/accessed/changed times — all re-read from disk at open time. With marks active, a summary row (`N item(s), files total X bytes` — shallow file sizes only) is appended. `Esc`/`Enter`/`q` closes |
| `=` | Diff the file under the cursor against the same-named file in the other pane's directory, shown as a colored unified diff (3 context lines; `+` green, `-` red, `@@` cyan, headers bold) in the built-in viewer — all the usual viewer keys (scroll, `/` search, `q` close) work as-is. Identical files, binary files, and a missing counterpart just log a note instead of opening. Reads are capped at the viewer's 10 MiB limit (`[truncated]` is flagged in the title) |
| `Y` (Shift+y) | Sync the active pane's **whole directory** onto the other pane's, after choosing a mode in a dialog: **Update copy** copies new/missing files only (a file is re-copied when sizes differ or the source is more than 1 s newer — the tolerance absorbs FAT-style mtime granularity; the destination is never deleted, and a newer destination file is left alone), or **Mirror**, which additionally deletes everything that exists only in the destination — mirror always shows an explicit deletion confirmation regardless of `confirm_operations`, and its deletions respect `delete_behavior` (trash by default). Runs as a cancellable background task with byte progress; symlinks are compared by link target and recreated as links, never followed. Same-directory and nested (either-way) pane pairs are rejected |

By default, `D`/`R` are also simultaneously bound to their lowercase forms (`d`/`r`). This is a concrete example of "multiple keys → one action" (you can add more of your own in the `[bindings]` section, described below).

Copy, move, and delete run as async tasks, with a progress gauge shown in the log pane. Other operations remain available while they run. `Ctrl+K` (`cancel_tasks`) cancels all running tasks together at any time. There is no UI for selecting and canceling individual tasks — it always targets "everything running right now." Canceled tasks are logged as `cancelled`, and whatever had already completed (e.g., files already copied) is left in place. If pressed while no tasks are running, only `no running tasks` is logged.

**Log pane timestamps:** Each log line is prefixed with the time it was recorded, in `YYYY-MM-dd HH:MM:SS` format (e.g., `2026-08-05 14:03:22`). Long messages (such as errors containing paths) wrap instead of being cut off at the right edge, and continuation lines are indented by the width of the timestamp (the timestamp itself is not repeated), so message columns line up and stay readable. When the log pane fills up, the latest content is always shown at the bottom, counted in post-wrap lines.

**Viewing the full log (`L`):** The log display below the status area shows only the last 4 lines, but pressing `L` (Shift+l) opens a full-screen log viewer where you can scroll through the entire log recorded during the session (up to roughly 500 lines, in memory), with the most recent line at the bottom. Scrolling and search use the exact same `less`-compatible fixed key set as the text viewer: `↑`/`↓` (also `k`/`j`) for one line, `PageUp`/`PageDown` (also `b`/`f`/`Space`) for a page, `d`/`u` for a half page, `Home` or `g` for the top, `End` or `G` for the bottom, `/`/`?` for forward/backward search, and `n`/`N` to move to the next/previous match (see "[Search (`less`-compatible)](#search-less-compatible)" above for search details — here the search target is each log line itself). `q`/`Esc` closes it (pressing `Esc` while a search is active only clears the highlight first, just like in the viewer). Since the log is in-memory only and not persisted, restarting `ozzel` also clears the log viewer's contents.

**Copy/move confirmation dialog:** With no name conflicts, copy and move show a single confirmation dialog before executing (enabled by default; can be disabled via `confirm_operations` in settings), like `Copy 3 item(s) -> /path/to/dest? (y/n)`.

**Same-name collision dialog (per file):** If the destination already has entries with the same names, a per-file dialog opens instead — one conflict at a time, titled `Overwrite? (2/5): name.txt`, showing both sides' size and modified time with `[New]` marking the newer one. The choices are:

| Choice | Effect |
| --- | --- |
| `Overwrite` | Transfer this entry over the existing one (directories merge, files are replaced) |
| `Rename` | Prompt for a different destination name for this entry (a name that also exists re-asks; it never overwrites through the rename path) |
| `Skip` | Leave this destination untouched; the source is not transferred |
| `Overwrite All` | Apply `Overwrite` to this and every remaining conflict |
| `Skip All` | Apply `Skip` to this and every remaining conflict |

`↑`/`↓` move the highlight, `Enter` answers, and `Esc` cancels the **whole** transfer (including the non-conflicting entries — nothing has started yet at that point). Non-conflicting entries transfer normally alongside whatever the dialog resolves. This dialog is itself the confirmation, so it appears even with `confirm_operations = false` — a collision is never silently overwritten.

**How `y` (copy path) works:** Without pulling in any extra dependency crates, it writes to the clipboard using a terminal escape sequence called [OSC 52](https://sw.kovidgoyal.net/kitty/clipboard/#clipboard-escape-code). Its advantage is that it works even over SSH or from inside tmux (if tmux is configured with the equivalent of `set-clipboard on`). On unsupported terminals, the escape sequence is simply ignored — no error, no crash (since there's no reliable way to detect support in advance, pressing `y` always logs `copied: /path/to/file`).

### Git Status

Inside a git work tree, each pane shows the current directory's git state, refreshed by a background `git status` run (via the `git` CLI on `PATH` — never a bundled library) on every directory change, `Ctrl+R`, and after every file operation:

- **A per-row marker column** (to the left of the mark column): `U` conflict, `M` modified, `A` added, `D` deleted, `R` renamed, `?` untracked. A directory row aggregates everything under it, showing the highest-priority state (`U` > `M` > `A` > `D` > `R` > `?`). In a huge repository only the pane's own subtree is scanned.
- **A `⎇ branch` cell in the pane header's second row** (the short commit hash when HEAD is detached).

Outside a work tree — or on a machine with no `git` at all — nothing is shown and nothing changes. The probes run detached from the task system: they never appear as running tasks, never gate quitting, and are not touched by `Ctrl+K`. When the pane is too narrow, the marker column is dropped before the name column gets squeezed (same policy as the permissions column). Set `show_git_status = false` in the config (also on the settings screen) to disable the probes entirely.

### Filtering & Search

| Key | Action |
| --- | --- |
| `f` / `/` | Start incremental filter mode |
| (while typing) `Enter` | Confirm the filter (the filter stays active; returns to normal mode) |
| (while typing) `Esc` | Cancel and clear the filter |
| In normal mode, `Esc` | Clear the filter if one is active |

In filter mode, whatever string you type is used directly as a case-insensitive substring match. Starting with `re:` treats everything after it as a case-sensitive regular expression (e.g., `re:^IMG_[0-9]+\.jpg$`). An invalid regex shows an error message and, without crashing, simply matches nothing.

### Prefix Jump (Forward-Match Incremental Search)

| Key | Action |
| --- | --- |
| `\` | Start prefix jump mode |
| (while typing) typing characters | Moves the cursor to the first entry matching the typed string (prefix match, case-insensitive) |
| (while typing) `Down` / `Tab` | Move to the next entry matching the same input (wraps to the top after the last one) |
| (while typing) `Up` | Move to the previous entry matching the same input (wraps to the bottom before the first one) |
| (while typing) `Enter` | End input; the cursor stays at its current position |
| (while typing) `Esc` | Cancel input; the cursor returns to where it was when the mode started |

Unlike the `f`/`/` filter, this never hides any entries in the list — it only ever moves the cursor. While there is no match, the cursor does not move, and a warning (`(no match)`) is shown in the input field. `..` (the entry for going up to the parent directory) is never a match target. This also works the same way inside a Virtual Directory (browsing inside a `.zip`, etc.).

### Filename Search (Recursive)

| Key | Action |
| --- | --- |
| `g` | Open the filename search popup (recursively searches under the active pane's directory) |
| (while typing) typing characters | Edit the search pattern (results update on every keystroke by default) |
| (while typing) `Up` / `Down` | Move the cursor in the result list |
| (while typing) `Enter` | Move to the parent directory of the selected entry, with the cursor placed on that entry |
| (while typing) `Esc` | Cancel the search and close it (the pane does not move) |

The pattern syntax is the same as the `f`/`/` filter: plain input is a case-insensitive substring match, and starting with `re:` treats the rest as a case-sensitive regular expression (e.g., `re:^main\.(rs|go)$`). An invalid regex shows an error message below the input field and simply matches nothing. Matching applies only to the filename (the last path component) — intermediate directory names are never matched. Directories themselves are included in the results (shown with a trailing `/`).

The searched tree is scanned exactly once, at the moment the popup is opened; from then on, every keystroke only re-matches against the in-memory snapshot (the disk is never rescanned on each keystroke). Hidden files follow the pane's display setting (the `.` key); if hidden files are not shown, hidden directories are not scanned either. Scanning stops at 100,000 entries, in which case `[truncated]` is shown in the title. Not available inside a Virtual Directory (while browsing inside an archive).

Setting `file_search_incremental = false` (or the "Behavior" category in the settings screen) turns off incremental updates. In that case, the previous results stay displayed while typing (the title shows `[Enter to search]`), the first `Enter` runs the search, and pressing `Enter` again once the results are already current moves to the selected entry.

### Zip Compression & Extraction

| Key | Action |
| --- | --- |
| `p` | Zip the marked entries (or the entry under the cursor if none are marked). A prompt for the archive name is shown, pre-filled with the first target's filename plus `.zip`. It is created in the other pane's directory |
| `u` | Extract the `.zip` file under the cursor into the other pane |

Compression (`p`) and this bulk extraction with `u` are currently zip-only (the Virtual Directory feature described below also supports formats other than zip).

### Virtual Directory (Browsing Archives Like Directories)

Pressing `Enter`/`o` (`open`) on an archive file with a supported extension lets you browse its contents in place, like a directory, without extracting it. No new keys are added — existing keys work inside the archive too (though their meaning shifts somewhat).

Supported formats:

| Format | Extension | Notes |
| --- | --- | --- |
| zip | `.zip` | Listed/extracted via random access from the central directory (as before) |
| tar | `.tar` | Uncompressed |
| tar+gzip | `.tar.gz` / `.tgz` | `flate2` (pure Rust) |
| tar+bzip2 | `.tar.bz2` / `.tbz2` | `bzip2` crate (uses the pure-Rust backend `libbz2-rs-sys` by default; no C library linking) |
| tar+xz | `.tar.xz` / `.txz` | `lzma-rs` (pure Rust, `#![forbid(unsafe_code)]`) |
| gzip (bare) | `.gz` (not `.tar.gz`) | Shown as a one-entry archive containing the decompressed payload (named after the file minus `.gz`). The size column shows gzip's recorded uncompressed size (inaccurate above 4 GiB) |
| bzip2 (bare) | `.bz2` (not `.tar.bz2`) | Same one-entry treatment; bzip2 records no uncompressed size, so the size column shows 0 until opened |

None of the above require a C toolchain (`cc`/system `libz`/`libbz2`/`liblzma`, etc.) — everything builds cross-platform with just cargo. 7z and rar are not supported.

**Behavior change note:** bare `.gz`/`.bz2` files used to open in the built-in viewer (as a hex dump of the compressed bytes); they now open as a one-entry Virtual Directory — press `Enter` again on the entry inside to view the decompressed content, or `C` to extract it to the other pane.

**Password-protected zips:** The listing opens without a password (it only reads the central directory's metadata). The first time the *contents* are needed — opening a file in the viewer, extracting with `C`, or unzipping with `u` — a masked password prompt appears; the password is verified on the spot (a wrong one logs `wrong password` and re-prompts) and, while browsing inside the archive, is remembered for the rest of that Virtual Directory session so several files can be opened with a single entry. Both AES-encrypted zips (7-Zip/WinZip's modern default) and legacy ZipCrypto ones are supported, via pure-Rust decryption (no C toolchain, same as everything else). Nothing is ever persisted. (Rare caveat: legacy ZipCrypto's integrity check lets roughly 1 in 256 wrong passwords through the initial verification; those fail during the actual extraction as a logged error.)

| Key | Action |
| --- | --- |
| `Enter` / `o` | Descend if it's a directory; if it's a file, open it in the built-in viewer (extracted into memory on the spot, without writing to disk) |
| `Backspace` | Go up one level inside the archive. Pressing it at the archive root exits the Virtual Directory back to the real directory, with the cursor restored to the original archive file's position |
| `Space` / `a` | Mark / mark all (as usual) |
| `f` / `/`, `s`, `.` | Filter, cycle sort, toggle hidden files (as usual, applied to the listing inside the archive) |
| `C` | Extract the marked entries (or the entry under the cursor if none are marked) **into the real directory in the other pane** (confirmation dialog and progress gauge work the same as a normal copy) |

The pane's header is shown in the form `archive-name:internal-path`, e.g., `archive.tar.gz:/internal/path`, so you can tell where you are inside the archive. zip listings come from the central directory; tar-family listings are synthesized by reading through the entire archive stream once. Even archives with no explicit directory entries (where only file entry paths are recorded) have their intermediate directories automatically filled in for display. Symbolic link entries inside tar-family archives are shown in the listing (but never followed); on extraction they are logged and skipped (see below).

**Tar-family formats have no central directory, so access is sequential.** Opening the listing or opening a file in the viewer always reads the stream from the beginning of the archive, so opening a large, highly compressed `.tar.gz`/`.tar.bz2`/`.tar.xz` can take a moment (up to a few seconds, depending on size) to list or open a file. zip is not subject to this limitation (direct access to the central directory makes it fast).

**A Virtual Directory is read-only.** The following operations are shown as errors in the log and are not executed: `M` (move, whether into or out of the archive), `D` (delete), `R`/`r` (rename), `K` (mkdir), `c` (duplicate), `p` (zip compression — recompressing the contents of an archive is not possible), `u` (unzip — nesting an archive extraction inside an archive is not possible), `e` (open in editor), `Shift+Enter` (open with the OS default app). `y` (copy path) is a specific exception and is allowed, copying in the form `archive.tar.gz:/internal/path`. `:` (command line) can be launched with the real directory containing the archive as the current directory. Moving outside the archive via a bookmark, the history menu, or `~` (home) automatically exits the Virtual Directory.

Opening an archive file that is itself inside an archive does not create a nested Virtual Directory — it is simply opened in the built-in viewer (usually as a hex dump, since it's binary). Opening a password-protected zip, or a corrupted or unsupported archive format, does not crash — it shows an error in the log.

### External Viewers per Extension (`[viewers]`)

The `[viewers]` section of the config file lets you specify an external command to use for `open` (`Enter`/`o`) per file extension. Extensions with no corresponding entry (or files with no extension) continue to open in the built-in viewer as before. `Shift+Enter` (`open_default`, opening with the OS default app) is unaffected by this setting.

```toml
[viewers]
md = "glow {}"   # Display Markdown with glow
log = "less {}"  # Display log files with less
jpg = "open {}"  # Open images with the OS default GUI app (example using macOS's `open` command)
```

The key is the lowercase, dot-less extension (matching is case-insensitive since the file side is lowercased first); the value is a shell command string. If `{}` appears in the command, the shell-quoted path is substituted there; if it doesn't, the quoted path is appended at the end. Execution uses the same TUI-suspension mechanism as `:`/`e`, but like `e`, there is no "press any key" pause — it returns to the filer immediately once the command finishes.

External viewers do not apply to files inside a Virtual Directory (browsing inside an archive), since no real file exists. Even if a matching `[viewers]` entry exists, this is logged and it falls back to the built-in viewer.

### History, Bookmarks & Home

| Key | Action |
| --- | --- |
| `H` (Shift+h) | Choose a directory to jump to from this pane's history (most-recently-visited order, up to 50 entries) |
| `b` | Choose a directory to jump to from the bookmark list |
| `B` (Shift+b) | Add the active pane's current directory to bookmarks (no duplicates are added) |
| `~` | Go to the `home` setting (or the OS home directory if unset) |

Inside the history/bookmark menus, the following fixed keys are available.

| Key | Action |
| --- | --- |
| `↑` / `↓` | Move the highlight |
| `Enter` | Move the active pane to the selected item |
| `Esc` | Close the menu (without moving) |
| `d` | (bookmark menu only) Delete the highlighted bookmark |

### Help Screen

| Key | Action |
| --- | --- |
| `h` / `?` | Open the current effective key binding list (help screen) |

A full-screen screen listing the **currently effective key bindings** — reflecting any user overrides via `[keys]`/`[bindings]` — grouped by category (navigation, marking, file operations, filtering, history/bookmarks/home, external integration/viewers, other). When multiple keys are assigned to the same action, they are combined into a single comma-separated line (e.g., `r, R    rename    Rename the cursor entry`). At the end, the fixed keys for each mode that are outside key-binding remapping (not remappable) — prompts, confirmation dialogs, the history/bookmark menu, the viewer, the log viewer, and the help screen itself — are also shown as a static section.

Scrolling and search use the exact same `less`-compatible fixed key set as the text viewer (and log viewer): `↑`/`↓` (also `k`/`j`) for one line, `PageUp`/`PageDown` (also `b`/`f`/`Space`) for a page, `d`/`u` for a half page, `Home` or `g` for the top, `End` or `G` for the bottom, `/`/`?` for forward/backward search, and `n`/`N` to move to the next/previous match (the search target here is the text of each line in the key binding list).

| Key | Action |
| --- | --- |
| `q` / `Esc` / `h` | Close the help screen and return to the filer (pressing `Esc` while a search is active only clears the highlight first, just like in the viewer) |

### Command Palette

| Key | Action |
| --- | --- |
| `F` (Shift+f) | Open the command palette |

A modal listing all actions (name + description); typing in the input field at the top filters both action names and descriptions with a case-insensitive substring match.

| Key | Action |
| --- | --- |
| `↑` / `↓` | Move the highlight |
| `Enter` | Run the highlighted action (the palette closes before running it, so actions that open a prompt or confirmation dialog work fine too) |
| `Esc` | Close the palette without running anything |
| Any other character key | Append to the filter string (editable with `Backspace`/`Delete`/`←`/`→`/`Home`/`End`) |

Handy for running an action by name when you don't remember its key binding, or for invoking rarely-used actions.

### Settings Screen

| Key | Action |
| --- | --- |
| `S` (Shift+s) | Open the settings screen |

A full-screen settings UI structured like `raspi-config`, with three levels: category → item → edit screen. `Esc` goes back one level at a time; pressing `Esc` at the category list closes the settings screen itself and returns to the filer.

| Category | Contents |
| --- | --- |
| Behavior | `confirm_operations` / `confirm_quit` / `quit_cd` / `mouse` / `delete_behavior` / `show_permissions` / `show_git_status` / `dim_inactive` / `file_search_incremental` / `command_line_interactive` |
| Colors | Each item under `[colors]` (`cursor` / `cursor_inactive` / `directory` / `hidden` / `executable`) |
| Startup/Integration | `home` / `editor` |
| Extension Viewers | `[viewers]` (list of extension → launch command; add/edit/delete) |
| Key Bindings | List of key bindings for every action; add/delete |

How to edit each item type:

- **ON/OFF items** (like `mouse`): Move the cursor to the item and press `Enter` to toggle it immediately.
- **Choice items** (`delete_behavior`): Choose between `trash`/`permanent` from a two-item list with `↑`/`↓`, and confirm with `Enter`.
- **Colors**: Choose from a pre-defined named color palette (each row shows a color swatch), or select "custom hex" at the end of the list to enter a `#RRGGBB` value directly.
- **Text items** (`home` / `editor` / a viewer's extension or command): Edit in a normal text input field, confirmed with `Enter`. Confirming with an empty field resets it to "unset."
- **Extension viewers**: The item list has a "+ add new" entry at the end; existing entries can be edited with `Enter` or deleted with `d`. In the edit screen, `Tab` switches between the extension field and the command field.
- **Key bindings**: Selecting an action shows all combos currently assigned to it. Pressing `a` captures the next key you press as the new combo, and shows a confirmation screen (`y`/`Enter` to confirm, `n`/`Esc` to cancel). If the captured combo is already assigned to a different action, the confirmation screen instead warns that it would be "taken from" that action; confirming automatically removes it from the original action. Pressing `d` deletes the combo under the cursor. The settings screen's own navigation keys (like `Esc`) are reserved and excluded from key-binding capture.

**Settings are saved on the spot.** Every time you edit and confirm an item, only the changed part is written to the [configuration file](#configuration) (using the `toml_edit` crate for diff-based writes, so existing comments and layout are preserved), and the in-app settings are then hot-reloaded immediately. If saving or reloading fails, the value shown on screen reverts to its previous (unchanged) state, and an error is logged. Since changes are saved as you go rather than "all at once when you close the screen," forgetting to close the settings screen never loses your changes.

Regarding where key bindings get written: new additions are always written to `[bindings]` (since `[bindings]` always takes priority over `[keys]`). Deletion removes the combo from `[bindings]` if it's there; otherwise (for a default key binding or one that came from `[keys]`), it writes that combo to `[keys]` as `"none"` — if a `[keys]` line for the same combo already existed, that line is simply overwritten, so the meaning is never left doubly defined. "Taking" a combo is just "delete" followed by "add," in sequence.

### Text Viewer

| Key | Action |
| --- | --- |
| `o` | Open the entry under the cursor in the built-in viewer. For a directory, instead of an error, it navigates into that directory (same as `Enter`, since both are unified into a single `open` action) |
| `Enter` (on a file) | Same as above (same behavior as `o`) |

The viewer is full-screen, temporarily replacing the entire filer display. It has two modes, text display and an `xxd`-style hex dump, toggleable at any time with `Tab`.

| Key | Action |
| --- | --- |
| `↑` / `↓` (also `k` / `j`) | Scroll one line (in hex mode, one line = 16 bytes) |
| `PageUp` / `PageDown` (also `b` / `f` / `Space`) | Scroll by page |
| `d` / `u` | Scroll by half page (same as `less`'s `d`/`u`) |
| `Home` / `g` | Go to the top |
| `End` / `G` (Shift+g) | Go to the bottom |
| `←` / `→` | Horizontal scroll (text mode only; there is no wrapping — content overflowing to the right is shown by scrolling) |
| `Tab` | Toggle between text display and hex dump display (the scroll position resets to the top when switching) |
| `/` | Start forward search input (see below) |
| `?` | Start backward search input (see below) |
| `n` | Move to the next match in the direction of the last search |
| `N` (Shift+n) | Move in the opposite direction of the last search |
| `q` | Close the viewer and return to the filer |
| `Esc` | Clears the highlight if a search is active (the viewer itself does not close). Otherwise, closes the viewer and returns to the filer |

Reads up to 10 MiB at most; files larger than that show only the beginning, with `[truncated]` shown in the footer. Byte sequences invalid as UTF-8 are displayed as `U+FFFD` (garbled but not crashing). If a NUL byte is found within the first 8 KiB, the file is treated as binary, and it is **not refused** — it is opened directly in hex dump mode (pressing `Tab` still lets you switch to a garbled-but-readable text display). Tabs are expanded to 4-space-equivalent column positions.

The footer shows the current mode, `[text]` / `[hex]`, plus, in text mode, the line range (e.g., `12-45/230 lines`), or in hex mode, the byte range (e.g., `192-320/512 bytes`). The hex dump format resembles `xxd`, as follows (an 8-digit offset + 16 bytes shown in hex, split into two groups of 8 + an ASCII gutter; unprintable characters shown as `.`).

```
00000010  48 65 6c 6c 6f 20 77 6f  72 6c 64 21 0a 01 02 03  |Hello world!....|
```

#### Search (`less`-compatible)

Pressing `/` or `?` opens a search input field at the bottom of the screen (with the same look as the `f`/filter input field). Press `Enter` to run the search.

- The typed string is first tried as a regular expression (case-insensitive); if it is invalid as a regex, it is instead treated as a plain substring match (matching `less`'s behavior).
- `/` (forward search) jumps to the first matching line at or after the current top line; `?` (backward search) jumps to a matching line at or before the current top line. If multiple matches are visible on screen, all of them are highlighted in reverse video.
- If no match is found before reaching the end of the file, it wraps around to the beginning (or end), and `(search wrapped)` is shown in the footer.
- `n`/`N` move to the next/previous match (`n` keeps the direction the search was started in, `N` reverses it). The footer shows the search string and `current position/match count` (e.g., `/needle  3/17`).
- Search in hex mode is performed against the string representation of the formatted hex dump lines as displayed on screen (including the offset, hex values, and ASCII gutter).
- Pressing `Esc` while typing a search cancels the input and restores whatever search state existed before (if any). Pressing `Esc` while a search is active (not while typing) only clears the highlight — the viewer stays open. Pressing `Esc` again closes the viewer.
- Matches that scroll off-screen due to horizontal scrolling (`←`/`→`) are still counted and highlighted when scrolled back into view.

Opening a file/directory with the OS default app (the `open_default` action) is bound to `Shift+Enter` by default (it can be freely reassigned to another key via `[keys]`).

**Important note about `Shift+Enter`:** Most terminals cannot distinguish `Shift+Enter` from a plain `Enter` unless they support the [kitty keyboard protocol](https://sw.kovidgoyal.net/kitty/keyboard-protocol/). `ozzel` queries once at startup whether the terminal supports this (`crossterm::terminal::supports_keyboard_enhancement`), and enables the `DISAMBIGUATE_ESCAPE_CODES` flag if it does. On supporting terminals (recent tmux, kitty, WezTerm, etc.), `Shift+Enter` works as `open_default`, but **on unsupported terminals, `Shift+Enter` arrives as a plain `Enter`, opening the built-in viewer** (no crash or misbehavior, but `open_default` never fires). When launching an external command or editor via `:` or `e`, this flag is temporarily disabled before handing control to the child process, and re-enabled on return (so that `Shift+Enter` detection doesn't break even after running something like `vim`).

### External Program Integration

| Key | Action |
| --- | --- |
| `:` | Show a command-line input prompt; on confirmation, suspends the TUI and runs the command via the shell (waits with "press any key" after it finishes, then returns) |
| `e` | Open the entry under the cursor in the configured editor (falls back to `$EDITOR` if unset). The TUI is suspended, but since the editor manages its own screen, control returns immediately once it exits |
| `,` | Open `ozzel`'s own config file in the editor (returns immediately, like `e`). If the file doesn't exist yet, it is created from a template identical to `examples/config.toml`, along with any necessary directories, before being opened |

If Ctrl+C is pressed inside the child process launched by `:`, only the child process is interrupted — `ozzel` itself keeps running (the child process is given its own process group and treated as the terminal's foreground group).

**The shell for `:` is launched in non-interactive mode** (unix: `$SHELL -c <command>`; Windows: `%COMSPEC% /C`). A non-interactive shell does not load `.zshrc`/`.bashrc`, so aliases and shell functions defined for interactive shells are not available by default. Setting `command_line_interactive = true` (or the "Behavior" category in the settings screen) launches it with `-i` (`$SHELL -i -c ...`) instead, loading the rc file so aliases and functions can be used. The trade-off is the rc-loading cost and side effects on every `:` command (prompt-initialization output, interactive aliases like `rm -i` becoming active, and possibly history-file writes, depending on the rc file). Do not enable this if your rc file contains startup logic like `exec tmux`. On Windows there is no equivalent concept, so this setting is ignored. Launching the editor via `e`/`,`, and commands under `[viewers]`, are always run non-interactively regardless of this setting.

**`,` determines the editor for opening the config file slightly differently from `e`.** It is decided in the order `config.editor` → `$EDITOR` → (if neither is set) `vim`, and unlike `e`, "no editor configured" is never an error (it would be self-defeating if the key for editing the config file didn't work without a config). When the editor exits, the config file is reloaded, and if parsing succeeds, key bindings, colors, delete behavior, and so on are applied on the spot, with `config reloaded` logged (no app restart needed). If the reloaded TOML is invalid, unlike other configuration errors, the app is not terminated — the error is logged and **the previous settings and key bindings continue to be used** (an invalid config at startup is a hard error, but a reload while running must not crash the app).

### Other

| Key | Action |
| --- | --- |
| `q` / `Ctrl+C` | Quit. Confirmed by default (can be disabled via `confirm_quit` in settings). If background tasks are running, it is always confirmed regardless of `confirm_quit` |

When `confirm_quit` (default `true`) is enabled, a "Quit ozzel? (y/n)" confirmation dialog is shown even if no tasks are running. Setting it to `false` quits immediately as long as no tasks are running (the "N task(s) running — quit anyway?" confirmation while tasks are running is always shown regardless of this setting).

Prompts (rename, mkdir, duplicate, zip name, command line) are shown as a popup box centered on screen, the same as the confirmation dialog (title = prompt name, input field, a hint line reading `Enter: OK   Esc: Cancel`). While typing, you can edit with `Backspace`/`Delete`/`←`/`→`/`Home`/`End`, all of which work correctly at the grapheme level, including for Japanese filenames. If you type a string longer than the input field, it scrolls horizontally within the box to follow the cursor position. `Enter` confirms, `Esc` cancels. In confirmation dialogs, `y`/`Y` executes, and anything else (including `Esc`) cancels.

By contrast, filtering (`f`/`/`), prefix jump (`\`), and viewer search (`/`/`?`) do not use this centered popup. These features filter the list or move the cursor in real time as you type, so it would be self-defeating for their target to be hidden behind a popup. As before, these are shown as a single-line input field at the bottom of the screen (where the status bar is).

### Making the Shell Follow `cd` on Exit (`--cwd-file`)

**Important: setting `quit_cd = true` and `--cwd-file` alone does not make the shell `cd`.** The child process (`ozzel`) has no way to change its parent shell's current directory. Only by defining a wrapper function like `oz()` below in your shell's rc file (e.g., `~/.zshrc`) and **launching `ozzel` through that wrapper instead of directly** does `cd` take effect. Launching plain `ozzel` (or `command ozzel`) directly does nothing — this is by design, not a bug.

Since `ozzel` is just a separate process, making the shell's own current directory follow the directory you navigated to inside the filer, once it exits, requires a bit of extra work (`cd` does not propagate from a child process to its parent shell). When `ozzel` is given the `--cwd-file <path>` flag, it writes the directory of the pane that had focus to `<path>` when it exits (through any exit path, including `q`/`Ctrl+C`) (it does not write anything if `quit_cd` is set to `false`; the default is `true`). If `--cwd-file` was not passed, nothing is written regardless of the value of `quit_cd`.

Here's an example wrapper function that uses this to make the shell's `cd` follow along. Add it to `~/.zshrc` or `~/.bashrc`.

```sh
oz() {
  local f
  f="$(mktemp)"
  command ozzel --cwd-file "$f" "$@"
  local d
  d="$(cat "$f" 2>/dev/null)"
  rm -f "$f"
  [ -n "$d" ] && [ -d "$d" ] && cd "$d"
}
```

From then on, use `oz` instead of `ozzel`, and the shell will also `cd` to that directory on exit.

Here is the equivalent function for PowerShell (Windows) (add it to `$PROFILE`).

```powershell
function oz {
    $f = New-TemporaryFile
    & ozzel --cwd-file $f.FullName @args
    $d = Get-Content $f.FullName -ErrorAction SilentlyContinue
    Remove-Item $f.FullName -ErrorAction SilentlyContinue
    if ($d -and (Test-Path $d -PathType Container)) {
        Set-Location $d
    }
}
```

## Mouse

Mouse capture is enabled by default (can be disabled via the `mouse` setting; described below). Behavior in normal mode is as follows.

| Action | Behavior |
| --- | --- |
| Left-click an entry row | Move focus to that pane, and move the cursor to that entry |
| Left-click a pane's header/margin area | Only moves focus (the cursor does not move) |
| Scroll the wheel over a pane | Moves only the cursor of the pane under the wheel, 3 lines per notch, without changing focus |
| Left-button drag from an entry row | A live rubber-band selection that toggles (marked → unmarked, unmarked → marked) the range from the drag's starting point to the current position, based on the mark state at the moment the drag started. Since the range is recomputed live as the pointer moves, even a row that was toggled while inside the range **automatically reverts to its pre-drag state** if the pointer moves back out of the range (it is not a permanent toggle — it's a temporary selection that only applies while inside the range). **If the pointer leaves the pane where the drag started, nothing is toggled and focus does not move in the other pane** (this is by design, so selections never leak across the left/right panes) |
| Double-click an entry row | Same as `open` (navigates if a directory, opens the viewer if a file) |

Other modal screens (prompts, confirmation dialogs, the history/bookmark menu, etc.) ignore mouse input and can only be closed with key operations like `Esc`. Click-to-dismiss is not implemented for any of these screens.

**Mouse capture is automatically suspended while the viewer, the log viewer (`L`), or the help screen (`h`/`?`) is open.** These are full-screen modes for reading file or log contents, so it's more practical to release capture and hand control back to the terminal's native mouse selection/copy. While suspended, all clicks and drags pass straight through to the terminal instead of ozzel, so you can select text with the mouse and copy it as usual (none of these three screens draw decorative characters like borders, so the selection never picks up anything other than the actual content). Returning to the filer by closing one of these three screens with `q`/`Esc` automatically resumes capture if `mouse = true` (the default). **As a trade-off, wheel scrolling doesn't reach ozzel in these three screens (since mouse capture is off) and instead scrolls the terminal's own scrollback.** Keyboard scrolling (`↑`/`↓`/`PageUp`/`PageDown`/`Home`/`End`, etc.) still works as usual. The command palette (`F`) is a screen for choosing what to act on rather than reading text, so it is not subject to this automatic release (mouse capture remains active while it's open).

Setting `mouse = false` never enables mouse capture at all, so the terminal's native text selection (drag-select and copy) is always available. If you want to use the terminal's native text selection while in the filer (normal mode) with `mouse = true` (the default), most terminals let you **hold Shift while dragging**. When suspending the TUI for an external program with `:`/`e`, the current mouse capture state is likewise temporarily released before handing off control, and restored on return, just like the keyboard-enhancement flag.

## Configuration

A TOML config file lets you change key bindings, delete behavior, whether copy/move need confirmation, the home directory, the editor, colors, and more. The following describes editing the config file directly. If you'd rather operate everything, including key bindings, from within the app, use the [settings screen](#settings-screen). Paths differ by OS (following CLI tool convention, even on macOS this uses an XDG-style path rather than Apple's standard `~/Library/Application Support`).

| OS | Path |
| --- | --- |
| Linux / macOS | `$XDG_CONFIG_HOME/ozzel/config.toml` (`~/.config/ozzel/config.toml` if `$XDG_CONFIG_HOME` is unset) |
| Windows | `%APPDATA%\ozzel\config\config.toml` |

If the config file does not exist, it starts with default settings. If the file exists but contains invalid TOML, an error message is shown at startup and the program exits (to prevent settings from being silently ignored without notice).

**Unknown keys are also errors.** For example, if you comment out a section heading (like `[viewers]`) while leaving one of its inner lines uncommented, that key is treated as an unrecognized top-level key. This used to be silently ignored, but it is now shown as an error at startup (and likewise on reload via `,`), along with the key name and a general hint: "did you forget to uncomment the section heading?" The contents of `[keys]`/`[bindings]`/`[viewers]` (key bindings and extensions) accept arbitrary names and are exempt from this check.

See [`examples/config.toml`](examples/config.toml) for a full example. Below is a description of every item.

```toml
# Delete behavior: "trash" (move to trash, default) or "permanent" (delete permanently)
delete_behavior = "trash"

# The home directory to move to with the `~` key. Defaults to the OS home directory.
# A leading `~` (either `~` alone, or `~/some/path`) is expanded, so you can also write
# home = "~/work" (the `~user` form is not supported)
home = "/Users/you/projects"

# The editor command used by the `e` key. Defaults to the $EDITOR environment variable
editor = "vim"

# Confirmation before running copy/move. Default is true (always confirm, like delete).
# Setting it to false executes immediately without confirmation, unless there's an
# overwrite conflict (conflicts are always confirmed even when false).
confirm_operations = true

# Confirmation on quit. Default is true (shows "Quit ozzel?" even with no tasks running).
# Setting it to false quits immediately without confirmation, as long as no task is
# running (the confirmation while a task is running is always shown regardless of this).
confirm_quit = true

# Whether to write out the focused pane's directory on exit when --cwd-file is given.
# Default is true. Setting it to false never writes anything out, even if
# --cwd-file is given.
quit_cd = true

# Whether to run filename search (g) on every keystroke. Default is true
# (incremental). Setting it to false only searches when you press Enter.
file_search_incremental = true

# Whether name sorting compares digit runs as numbers (file2 < file10).
# Default is true. Setting it to false restores strict lexicographic order.
natural_sort = true

# Whether single-step cursor movement (Up/Down) wraps around at the list
# edges. Default is false. PageUp/PageDown, Home/End, and the mouse wheel
# always stop at the edges regardless of this setting.
cursor_wrap = false

# How the size column renders sizes: "human" (1.5M, default), "bytes"
# (1536), or "bytes_grouped" (1,536). The v key cycles these at runtime
# and writes the choice back here.
size_format = "human"

# Whether to run the : command in an interactive shell ($SHELL -i -c). Default is
# false ($SHELL -c). Setting it to true loads .zshrc/.bashrc, making interactive-shell
# aliases and functions available from :  (be aware of the rc-loading cost and side effects).
# unix only. Ignored on Windows.
command_line_interactive = false

# Whether to enable mouse capture. Default is true (click to focus/move the cursor,
# wheel scrolling, drag to toggle-mark a range, double-click to open). Setting it to
# false never enables it, leaving the terminal's native text selection available as-is.
mouse = true

# Whether to show the permissions column (unix: drwxr-xr-x format, Windows: simplified
# display) on each row. Default is true. If the pane is too narrow, it is automatically
# hidden before the name column gets squeezed.
show_permissions = true

# Whether to show the git status marker column and the ⎇ branch header cell inside
# a git work tree, refreshed by a background `git status` run. Default is true.
# Outside a work tree nothing is shown either way; false disables the probes entirely.
show_git_status = true

# External viewer per extension. When opening with open (Enter/o), an external command
# can be specified per extension to use instead of the built-in viewer. The key is the
# lowercase, dot-less extension; the value is a shell command. If "{}" appears in the
# command, the path is substituted there (shell-quoted); otherwise it's appended at the
# end. Extensions with no corresponding entry continue to open in the built-in viewer
# as before. Does not apply to files inside a Virtual Directory (browsing inside an
# archive), since no real file exists there — it always falls back to the built-in
# viewer.
# [viewers]
# md = "glow {}"    # Display Markdown with glow
# log = "less {}"   # Display log files with less
# jpg = "open {}"   # Open images with the OS default GUI app (macOS example)

# Cursor row color. Accepts a named color (case-insensitive; "light_green" and
# "lightgreen" are the same) or a "#RRGGBB" hex value. An invalid value is a
# startup error, like other config errors.
# Default is light green (#90EE90). On terminals without true-color support,
# using a named color (e.g., "light_green") is recommended.
[colors]
cursor = "#90EE90"
# Cursor row color for the inactive pane (accepts the same values as cursor). Default is "white"
cursor_inactive = "white"
# Whether to dim the inactive pane. Default is true.
# Normal rows are dimmed using the terminal's "dim" display (SGR dim), but the
# cursor row alone darkens the background color itself toward black (since many
# terminals apply SGR dim to background colors weakly or not at all, without
# darkening the cursor_inactive color directly the inactive-side cursor alone
# would stay bright). Even when darkened, the cursor row is still the only row
# with a background, so its position remains identifiable.
dim_inactive = true

# Row colors by type (applied to rows other than the cursor row; accepts the same
# values as cursor). If a row matches multiple types (e.g., a hidden directory),
# only one color is used, in the priority order hidden > executable > directory.
# The color for marked rows (yellow) always takes priority over these.
directory = "cyan"    # Directories (default: cyan)
hidden = "red"        # Hidden files/directories (default: red)
executable = "yellow" # Executable files (unix: x bit, Windows: extension. default: yellow)

# Key binding overrides (method 1). Left side is a key combo, right side is an action name (snake_case)
[keys]
"C-c" = "copy"     # Example: assign Ctrl+C to copy (note this conflicts with the default quit key)
"q" = "none"       # Specifying "none" unassigns that key

# Key binding overrides (method 2, more ergonomic). Left side is an action name,
# right side is an array of key combos. Each listed key is assigned to that
# action (overriding it if another action already had it). This is applied
# after [keys], so if both mention the same key, [bindings] wins. There is
# no "none" (unassign) here like in [keys] — continue to use [keys] for that.
[bindings]
rename = ["r", "S-r"]   # Example matching the default (multiple keys for one action)
delete = ["d", "S-d"]
```

The [`examples/config.toml`](examples/config.toml) that actually gets generated has, in its `[bindings]` section, not just a few examples like the above, but **every default key binding written out one per action** (from `cursor_up = ["Up", "i"]` to `quit = ["C-c", "q"]`). All of them are valid lines, so using them as-is behaves no differently from the defaults — the point is to let you see every key binding at a glance and edit whichever line you like directly. **Removing a line does not remove that key's assignment** (the built-in default in the code remains in effect). To actually disable a specific key, rather than deleting this line, specify `"that-key" = "none"` in `[keys]`.

### Key Notation

- Modifier keys can be stacked as prefixes: `C-` (Ctrl), `S-` (Shift), `A-` (Alt) (e.g., `C-r`, `S-tab`).
- Named keys: `up` / `down` / `left` / `right` / `space` / `tab` / `backspace` / `enter` / `esc` / `home` / `end` / `pageup` / `pagedown` / `delete`
- Anything else is treated as a single-character literal (e.g., `a`, `.`, `~`). Uppercase letters (e.g., `R`) are automatically treated so that they match the actual keystroke including the Shift modifier.

### Specifying Colors

- Named colors: `black`, `red`, `green`, `yellow`, `blue`, `magenta`, `cyan`, `gray`/`grey`, `dark_gray`/`darkgray`, `light_red`, `light_green`, `light_yellow`, `light_blue`, `light_magenta`, `light_cyan`, `white`, etc. (case-insensitive, and indifferent to the presence of `_`/`-`/spaces)
- Hex colors: `#RRGGBB` (e.g., `#90EE90`)

### Action Names (valid values for the right side of `[keys]` / the left side of `[bindings]`)

`cursor_up`, `cursor_down`, `focus_left`, `focus_right`, `page_up`, `page_down`, `top`, `bottom`, `switch_pane`, `open`, `parent`, `cycle_sort`, `sort_dialog`, `toggle_size_format`, `calc_dir_size`, `toggle_hidden`, `swap_panes`, `refresh`, `mark`, `mark_all`, `rename`, `rename_marks`, `mkdir`, `delete`, `copy`, `move`, `duplicate`, `copy_path`, `filter`, `clear_filter`, `jump_search`, `file_search`, `zip_marked`, `unzip`, `cancel_tasks`, `symlink`, `chmod`, `touch`, `file_info`, `diff`, `sync_dirs`, `history_jump`, `history_back`, `history_forward`, `bookmark_jump`, `bookmark_add`, `go_home`, `command_line`, `open_editor`, `open_default`, `help`, `edit_config`, `show_log`, `function_list`, `settings`, `quit`

You can always check the current effective key bindings corresponding to this list from the in-app help screen (`h`/`?`).

## Data Files

Directory history and bookmarks are persisted as JSON files.

| OS | Path |
| --- | --- |
| Linux / macOS | `$XDG_DATA_HOME/ozzel/` (`~/.local/share/ozzel/` if `$XDG_DATA_HOME` is unset) |
| Windows | `%APPDATA%\ozzel\data\` |

- `history.json`: Visited directories per pane (up to 50, deduplicated, most-recent order)
- `bookmarks.json`: The bookmark list (in the order added)
- `sort_prefs.json`: Remembered per-directory sort choices (up to 200, most-recently-changed order; see "Per-directory sort memory" above)

If either file is missing, it starts in an empty state. If a file is corrupted (unparseable), it also falls back to an empty state, and this is shown in the log (never silently ignored). Bookmarks are saved on every change; history is saved on exit.

## Known Limitations

- **If `quit_cd` doesn't seem to work, the cause is almost certainly a missing wrapper.** `quit_cd = true` (the default) and `--cwd-file` alone will not make the shell `cd` when launching plain `ozzel`. Define the [shell wrapper function above](#making-the-shell-follow-cd-on-exit---cwd-file) in your rc file and launch through that wrapper instead (see that section for details).
- **zip assumes UTF-8 only.** Extracting a zip containing entries with Shift_JIS (CP932) names may result in garbled filenames. If the raw byte sequence of a name is invalid as UTF-8, a warning is logged during extraction, but no conversion is performed.
- **The display width of East Asian ambiguous-width characters (like ①) may be off.** This is a currently accepted known limitation.
- **Symbolic links have an intentionally asymmetric design: followed by navigation operations, but not by mutating operations.** Symbolic links pointing to directories are treated the same as directories in navigation/browsing contexts (descending with `Enter`/`o`, sorting, color, `<DIR>` size display, `\`/filter/mark). Entering a link with `Enter` sets the current directory to the link's own path (e.g., `/a/link`) (it is not normalized to the link target's real path). Going back with `Backspace` from there naturally returns to the link's parent directory (`/a`). A symbolic link pointing to a file shows the target's contents in the built-in viewer via `Enter`/`o` (`[viewers]` extension matching first tries the link's own name, falling back to the target's extension if there's no match). The executable color is determined by the target's permissions. A broken link (whose target doesn't exist) cannot be navigated into or previewed; trying to open it logs an error. **Copy, move, delete, duplicate (`c`), and zip compression, on the other hand, act only on the link itself and never follow the target** (copying a link produces a copy that is itself a link pointing to the same place; deleting a link leaves the target untouched). In the listing, symbolic links are marked with a trailing `@` on the name to distinguish them (same convention as `ls -F`). A symbolic link entry is stored as a link when zipping, but on extraction, symbolic link entries are, for safety, not restored — they are logged and skipped instead.
- **Trash support on Linux assumes an XDG-compliant desktop environment.** On unsupported environments, it fails and shows an error (it never silently falls back to permanent deletion).
- **Trash on macOS goes through `NSFileManager`.** The `trash` crate's default (the equivalent of requesting "Move to Trash" via the GUI) requires Automation permission not granted to the terminal app, which can fail with an error in terminals that don't prompt for that permission. `ozzel` explicitly chooses the `NsFileManager` deletion method, so it works without needing any additional permissions.
- **The viewer shows only the beginning of files larger than 10 MiB.** The binary-detection heuristic (choosing the initial display mode, text or hex) only looks at NUL bytes within the first 8 KiB, so a file with NUL bytes only further in may be incorrectly opened in text mode (either way, you can always switch modes manually with `Tab`).
- **`Shift+Enter` only works as `open_default` if the terminal supports the kitty keyboard protocol.** On unsupported terminals, it arrives as a plain `Enter`, opening the built-in viewer instead (see "External Program Integration" above).
- **A Virtual Directory does not recursively browse nested archives (an archive inside an archive).** Opening a `.zip`/`.tar.gz`/etc. from within an archive simply opens it in the built-in viewer (usually as a hex dump, since it's binary).
- **Tar-family archives do not support 7z or rar.** Only gzip, bzip2, and xz compression are supported; any other `.tar.*` (e.g., zstd, lz4) is not recognized and is opened as a regular file in the built-in viewer.
- **Password-protected zip support has caveats.** Viewing and extracting work via a masked password prompt (see "Virtual Directory" above), but zip *creation* (`p`) never encrypts, and legacy ZipCrypto's weak integrity check lets roughly 1 in 256 wrong passwords past the initial verification (they still fail, as a logged error, during the actual read).
- **The settings screen does not support mouse operations.** Every category, item, and edit screen requires keyboard input only.
- Unusual regex syntax and highly unusual filenames (e.g., containing NUL bytes) have not been explicitly tested.
- **`ozzel update` does not work until the repository is published on GitHub.** Both checking the remote version and reinstalling via `cargo install --git` fail, and a corresponding error message is shown (it does not crash).

## License

MIT License. See [LICENSE](LICENSE) for details.
