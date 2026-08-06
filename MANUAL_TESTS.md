# Manual Test Checklist

A list of manual smoke tests covering items that are hard to verify with automated tests (`cargo test`) — rendering in a real terminal, TUI suspend/resume, signals, interaction with child processes, and so on. The items are organized by area, based on what was actually checked in each phase during development.

Run `cargo run --` in a new terminal (or a multiplexer such as `tmux`) and go through the following in order. Having a directory with Japanese file names on hand makes it easier to check width-calculation behavior.

## 1. Browsing

- [ ] Launching with `ozzel [left dir] [right dir]` opens both panes in the specified directories
- [ ] Omitting the arguments opens both panes in the current directory
- [ ] Specifying a nonexistent directory shows an error message and exits (without breaking the terminal)
- [ ] `↑` / `↓` move the cursor. Same for `PageUp` / `PageDown` / `Home` / `End`
- [ ] `i` / `k` (new) move the cursor the same way as `↑` / `↓`
- [ ] `Shift+↑` / `Shift+↓` jump to the top/bottom of the list (same behavior as `Home`/`End`)
- [ ] `Tab` switches the active pane, and the border thickness changes
- [ ] `←` moves focus directly to the left pane, `→` to the right pane. Nothing happens if that side is already active
- [ ] `j` / `l` (new) also move focus directly to the left/right pane, same as `←` / `→`
- [ ] `K` (Shift+k) opens the mkdir prompt (confirm that lowercase `k` is dedicated to cursor movement and does not open mkdir)
- [ ] `Enter` enters a directory. `Enter` on `..` goes back to the parent
- [ ] Right after going back to the parent, the cursor is restored to the position of the directory just left
- [ ] `Backspace` goes back to the parent directory
- [ ] `s` cycles the sort key through Name → Size → Modified time → Extension → Name. Directories always come above files
- [ ] `.` toggles showing/hiding hidden files (dotfiles)
- [ ] `w` swaps the contents of the left and right panes
- [ ] `Ctrl+R` reloads both panes
- [ ] Japanese file names (e.g. `日本語ファイル名.txt`) display without misaligning columns relative to other rows
- [ ] When the window width is narrow, paths and file names are truncated appropriately with an ellipsis (`…`)
- [ ] The cursor row is shown with a light green background and black text (with default settings). Even if a marked row is at the cursor position, it can still be distinguished by the `*` marker
- [ ] The cursor row of the inactive pane is shown with a white background and black text (with default settings). The active side remains green
- [ ] The rows, border, and title of the inactive pane are dimmed (default `dim_inactive = true`). The inactive pane's cursor row (white background) is dimmed too, but since only that row has a background, its position remains distinguishable
- [ ] **(New) The inactive-side cursor row's background color is actually a dark color.** Switch the active pane with `Tab` and compare both panes: confirm the inactive side's cursor row background is not just "a light white" but a clearly dark gray (terminal SGR dim has almost no effect on background colors, so previously it looked like the same bright white background as the active side — check this fix). Also confirm the active side's cursor row color remains at the brightness configured, unchanged
- [ ] Even after changing `cursor_inactive` in `[colors]` to a vivid color (e.g. `"#00FF00"`), confirm the inactive pane's cursor row is shown as a darkened version of that color (not at the configured brightness as-is, and not turned pure black either)

## 2. Marking & Synchronous Operations

- [ ] `Space` marks the cursor position (shown as `*`, yellow) and moves the cursor down by one
- [ ] Pressing `Space` on `..` does nothing
- [ ] `a` toggles the mark state of all currently displayed entries
- [ ] `R` opens the rename prompt pre-filled with the current name; confirming actually renames it
- [ ] `r` (lowercase) also opens the same rename prompt (`R`/`r` are multiple key bindings for the same action)
- [ ] `K` opens the mkdir prompt; confirming creates a new directory
- [ ] `D` shows a delete confirmation dialog (with the target count). `y` deletes, `n`/`Esc` cancels
- [ ] `d` (lowercase) also opens the same delete confirmation dialog
- [ ] When `delete_behavior` is set to `trash`, items move to the trash (confirm that without a desktop environment it fails and shows an error, rather than silently falling back to permanent deletion)
- [ ] **macOS:** Create a suitable file in a scratch directory, delete it with `D`/`y`, and confirm no error such as `The AppleScript exited with error` appears, that the log shows "moved 1 item(s) to trash", and that the file has actually moved into `~/.Trash/` (check with `ls ~/.Trash/`). Also confirm no Automation permission prompt appears in the terminal

## 3. Async Tasks & Progress Display

- [ ] `C` shows a confirmation dialog for copying the marked entries (or the entry at the cursor, if none are marked) to the other pane (`Copy N item(s) -> /dest? (y/n)`). `y` starts it, and a gauge appears in the log area right away
- [ ] Copy a fairly large directory (tens of thousands of files) and confirm the gauge percentage increases monotonically
- [ ] Other operations, such as moving the cursor, remain possible while a copy is in progress (the UI is not blocked)
- [ ] Start two transfers in a row (confirming each with `y`) and confirm both gauges progress simultaneously and independently
- [ ] `M` similarly shows a confirmation dialog for moving (`Move N item(s) -> /dest? (y/n)`) before running it asynchronously
- [ ] When the copy/move destination has entries with the same name, they are combined into a single confirmation dialog including the overwrite count (`Copy N item(s) -> /dest? (M will be overwritten) (y/n)`. It must not become two separate dialogs)
- [ ] Pressing `n` on a copy/move confirmation cancels it, and confirm no task starts at all
- [ ] Set `confirm_operations = false` and confirm that copies/moves with no conflicts run immediately without confirmation
- [ ] Even with `confirm_operations = false`, confirm that copies/moves with conflicts still always show a confirmation dialog (this is the one case that is always confirmed)
- [ ] After a task completes, both panes reload automatically, marks are cleared, and a result summary is shown in the log
- [ ] Pressing `q` while a task is running shows a confirmation dialog including the number of running tasks. `y` quits, `n` continues

### Quit confirmation `confirm_quit` (new)

- [ ] With no task running, pressing `q` (or `Ctrl+C`) shows a "Quit ozzel? (y/n)" confirmation dialog by default (`confirm_quit = true`). Canceling with `n`/`Esc` returns to the filer and the app does not quit
- [ ] Pressing `y` in the same state actually quits
- [ ] Set `confirm_quit = false` and confirm that, with no task running, pressing `q` quits immediately with no confirmation
- [ ] Even with `confirm_quit = false`, confirm that pressing `q` while a task is running still shows the "N task(s) running — quit anyway?" confirmation (this confirmation always appears regardless of the `confirm_quit` setting)

### Log timestamps & wrapping (new)

- [ ] Perform some operation (e.g. adding a bookmark with `B`) and confirm the log line is prefixed with a timestamp in `YYYY-MM-dd HH:MM:SS ` format (including the year, e.g. `2026-08-05 14:03:22`)
- [ ] Narrow the terminal width (to around 80–90 columns), trigger an error with a long path (e.g. adding a bookmark with `B` in a deep directory, or a delete/rename error with a long path), and confirm no characters are cut off at the right edge of the log area — the full text wraps across multiple lines and remains readable
- [ ] In the wrapping above, confirm that from the second line onward the indentation matches the width of the timestamp (spaces only, the time itself is not repeated), and that the message body columns line up with the first line
- [ ] Confirm that even long log lines containing Japanese text wrap correctly without splitting full-width characters in half
- [ ] When the wrapped line count exceeds the height of the log area, confirm the latest content is always shown at the bottom (older content scrolls off the top) and new log lines are never buried
- [ ] Perform an operation long enough to fill the entire height of the log area (e.g. repeatedly pressing `B`) and confirm the log area still wraps correctly within its remaining line count even while a gauge is displayed (also confirm the gauge line itself has no timestamp)

## 4. Filtering & Search

- [ ] Pressing `f` or `/` enters filter mode, and the list is filtered in real time as each character is typed
- [ ] The pane header shows `[flt: <input string>]`
- [ ] Input starting with `re:` is treated as a regular expression (e.g. `re:^IMG_[0-9]+\.jpg$`)
- [ ] Entering an invalid regular expression turns the input field red and shows an error message. The list is treated as "no matches" and does not crash
- [ ] `Enter` confirms the filter (the list stays filtered and returns to normal mode)
- [ ] Pressing `Esc` in normal mode clears an active filter
- [ ] `..` is always shown even while filtering

## 5. Zip Compression & Extraction

- [ ] `p` opens a prompt for zip-compressing the marked entries (or the entry at the cursor, if none are marked), pre-filled with `<target-name>.zip`
- [ ] Confirming creates the archive in the other pane's directory
- [ ] Nested directories and directories containing Japanese file names can be compressed
- [ ] If an archive with the same name already exists, an overwrite confirmation dialog appears
- [ ] `u` extracts a `.zip` file into the other pane. Pressing it on a non-`.zip` file results in an error
- [ ] If the extraction destination already has a top-level entry with the same name, an overwrite confirmation dialog appears
- [ ] The compressed/extracted tree matches the original content (file contents, Japanese file names)

## 6. History, Bookmarks & Home

- [ ] `B` adds the active pane's current directory to bookmarks and shows this in the log. Pressing it again on the same directory logs that it is "already registered"
- [ ] `b` shows a bookmark list popup in the center of the screen
- [ ] In the bookmark list, `↑`/`↓` move the selection, and `Enter` moves the active pane to the selected directory
- [ ] Pressing `d` in the bookmark list deletes the highlighted entry and updates the list in place
- [ ] Closing the bookmark list with `Esc` does not move the pane
- [ ] `H` (Shift+h) shows the history list, with recently visited directories ordered newest first (**key layout change**: the previous `h` binding is now assigned to the help screen — checked in the next section)
- [ ] `~` moves to the home directory (the config's `home`, or the OS home if unset)
- [ ] Pressing `H` (Shift+h) does not move to home (confirming the help screen has not taken over the old home key — it is now dedicated to history)
- [ ] With a nonexistent `home` configured, pressing `~` logs an error and the current location does not change

## 7. Help Screen (new)

- [ ] Pressing `h` opens the help screen full-screen (the top/bottom panes, log, and status bar temporarily disappear)
- [ ] Pressing `?` also opens the same help screen
- [ ] Confirm the list is shown with headings by category (Navigation, Marking, File Operations, Filtering, History/Bookmarks/Home, External Integration/Viewer, Other)
- [ ] Confirm the `rename` row shows both `r` and `R`, comma-separated (checking the display for multiple keys mapped to one action)
- [ ] Confirm that a static section at the end of the list describes the fixed, non-remappable keys (prompts, confirmation dialogs, history/bookmark menus, viewer, help screen itself)
- [ ] Confirm scrolling works with `↑`/`↓`/`PageUp`/`PageDown`/`Home`/`End` (and `g`/`G`), without going past the top or bottom
- [ ] Confirm the help screen closes and returns to the filer with any of `q`, `Esc`, or `h`
- [ ] Set an override such as `"z" = "quit"` in `[keys]` and confirm the help screen's `quit` row shows `z` (confirming it reflects the currently effective key bindings)
- [ ] Confirm the `edit_config` row is shown in the list with the `,` key (in the "External Integration/Viewer" category)
- [ ] Confirm the `open` row is shown as a single line, with its keys grouped comma-separated like `Enter, o` (not split into two lines for the old `enter`/`view`)

## 8. Viewer (Text / Hex Dump)

- [ ] `o` opens the file at the cursor position in the built-in viewer (full-screen display; the top/bottom panes, log, and status bar temporarily disappear). Pressing `Enter` on a file also opens the viewer (confirming `Enter`/`o` are the same single `open` action)
- [ ] Confirm `x` is not bound to anything (pressing it does nothing) (this was the old View action's key, freed up by merging it into `open`)
- [ ] Open a long Rust source file or similar and confirm scrolling works with `↑`/`↓`/`PageUp`/`PageDown`/`Home`/`End` (and `g`/`G`), without scrolling past the top or bottom
- [ ] Confirm `j`/`k` (1 line), `d`/`u` (half page), and `f`/`b`/`Space` (1 page) scroll by the same amount as `↓`/`↑` and `PageDown`/`PageUp` respectively (`less`-compatible scroll keys, new)
- [ ] Confirm the footer shows the current mode, current position, and total line count, e.g. `path  [text]  [12-45/230 lines]  Tab:hex/text  q:close`, updating as you scroll
- [ ] Open a file with a Japanese file name and Japanese text and confirm the characters display correctly (columns do not misalign)
- [ ] In a file with long lines, confirm `←`/`→` scroll horizontally (confirm this is toggling the overflow display, not wrapping)
- [ ] With a text file open, pressing `Tab` switches to hex dump display (`[hex]`), and the footer changes to a byte range display (e.g. `[0-320 bytes]`). Pressing `Tab` again returns to text display
- [ ] Confirm the hex dump display matches `xxd`'s appearance (8-digit offset, 16 bytes shown as two groups of 8 in hex, an ASCII gutter with `|...|` on the right, non-printable characters shown as `.`). Comparing against `xxd <file>` is a good way to check
- [ ] Confirm scrolling in hex dump display with `↑`/`↓`/`PageUp`/`PageDown`/`Home`/`End` works in units of 1 line = 16 bytes, without going past the top or bottom
- [ ] Opening a binary file (an executable or image, etc.) is not rejected and opens directly in hex dump mode. Confirm pressing `Tab` switches to text display (garbled but displayed)
- [ ] Confirm pressing `o` (or `Enter`) on a directory moves into that directory without an error (new behavior after `open` merged `view`/`enter` — previously `View` errored here)
- [ ] Confirm `q` or `Esc` closes the viewer and correctly returns to the filer screen (no display corruption). Confirm the return display is not corrupted regardless of whether it closes from text or hex mode
- [ ] Confirm `open_default` (formerly: open with the OS default app) works when bound to a free key such as `x` (e.g. `"x" = "open_default"` in `[keys]`)

## 9. External Program Integration (TUI Suspend/Resume)

This item is not covered by automated tests. Be sure to check it in a real terminal (`tmux` recommended).

- [ ] `:` opens the command input prompt. Pressing `Enter` with an empty input cancels without running anything
- [ ] `: ls` → `Enter` suspends the TUI and shows `ls`'s output as-is. After it runs, `[ozzel] exit: ... — press any key` is shown, and pressing any key correctly resumes the TUI (no display corruption)
- [ ] `: vim <filename>` → `Enter` launches vim full-screen, and you can edit normally. Quitting with `:q` returns to the TUI via the "press any key" prompt
- [ ] `e` opens the file at the cursor directly in the editor (`config.editor` or `$EDITOR`). Quitting returns to the TUI immediately (without going through "press any key")
- [ ] Pressing `e` on a directory results in an error. Same when no file is selected (e.g. on `..`)
- [ ] With no editor configured and no `$EDITOR` set, pressing `e` logs an error message
- [ ] Run a command that takes a while, such as `: sleep 30`, and press `Ctrl+C` partway through. **Confirm only the child process is interrupted and `ozzel` itself survives, showing the "press any key" prompt** (checking whether the process group is correctly separated — this is prone to regressions, so be sure to check it)
- [ ] While a copy task is running in the background, open a child process/editor with `:` or `e`, wait a while, then quit. Confirm the display is not corrupted after returning, and that the progress gauge correctly resumes and continues

### `,` (edit_config) and config live reload (new)

- [ ] With no config file present, pressing `,` creates a new one (including parent directories) from a template identical to `examples/config.toml`, and opens it in the editor (`config.editor` → `$EDITOR` → falling back to `vim` if unset)
- [ ] Confirm the opened template's `[bindings]` section writes out every default key binding as an active line, one per line, from `cursor_up = ["Up", "i"]` through `quit = ["C-c", "q"]` (not just a single commented-out example)
- [ ] Edit the `quit` line in `[bindings]` to add a key, e.g. `quit = ["C-c", "q", "Z"]`, and confirm that saving with `:wq` shows "config reloaded" and the added key (`Z`) immediately quits the app
- [ ] Confirm `,` falls back to `vim` and opens fine even with both `editor`/`$EDITOR` unset (`e` errors in this case, but `,` must not)
- [ ] Change the cursor color (`[colors] cursor`) in the config file, save and quit with `:wq`, and confirm "config reloaded" appears in the log and the new cursor color takes effect immediately **without restarting the app**
- [ ] In the same edit, add a new key binding to `[keys]` (e.g. `"z" = "quit"`) and confirm the key works immediately after saving (also confirm the new binding shows up in the help screen via `h`)
- [ ] Press `,` again, edit it into invalid TOML (e.g. remove a closing bracket), and save with `:wq`. Confirm the app does not exit, an error message is shown in the log, and the config that was in effect just before (the changed color and key binding) is kept as-is
- [ ] Confirm pressing `,` for a config file that already exists does not overwrite or reset its contents

### `Shift+Enter` / kitty keyboard protocol

- [ ] **Check in tmux (with kitty protocol support):** confirm pressing `Shift+Enter` on a file triggers `open_default` (open with the OS default app) (confirming the difference from `Enter` alone, which opens the built-in viewer)
- [ ] In the same tmux session, confirm `C`/`M`/`D`/`R`/`K` (the default uppercase/Shift-modified key bindings) all still work as before (a regression check that Phase 2's uppercase+SHIFT normalization is not broken even with the kitty protocol's `DISAMBIGUATE_ESCAPE_CODES` flag enabled)
- [ ] **Check in a terminal without kitty protocol support (e.g. macOS's stock Terminal.app, or a raw xterm without tmux):** confirm pressing `Shift+Enter` arrives as a plain `Enter` and opens the built-in viewer (`open_default` does not fire). Confirm no crash or misbehavior
- [ ] Right after resuming the TUI in tmux via `: vim <filename>` → edit → `:q`, confirm `Shift+Enter` works the same as before suspension (checking whether the flag push/pop has become asymmetric — if broken, `Shift+Enter` stops working after resume, or normal key input gets garbled)
- [ ] In the same just-resumed state, confirm `C`/`M`/`D`/`R`/`K` still work too

**Confirming no flag leaks after exit (regression test, new):** the kitty keyboard protocol keeps separate flag stacks for the main screen (the normal screen) and the alternate screen (the one ozzel uses), so a single mistake in push/pop timing can leave a flag set in the shell even after ozzel exits (if this happens, pressing something like `Ctrl+A` will type a raw string such as `7;5u` straight into the shell). Be sure to check this in a terminal that supports the kitty protocol (**Ghostty**, kitty itself, a supporting version of WezTerm, etc.) — ideally check both launching directly without tmux and via a supporting tmux.

- [ ] Launch `ozzel`, quit right away with `q`/`y` without doing anything. Right after quitting, press `Ctrl+A` (or another Ctrl-modified key) and confirm the shell prompt responds normally (e.g. the cursor moves to the start of the line). Confirm no mystery string containing `u` (such as `7;5u`) gets typed into the prompt
- [ ] Open and copy a few files inside `ozzel`, then quit, and confirm `Ctrl+A` etc. still work normally the same way
- [ ] Launch an external command/editor at least once via `:`/`e` (e.g. `: vim <filename>`), then quit `ozzel`, and confirm `Ctrl+A` etc. still work normally (a regression check for the case that goes through suspend/resume)
- [ ] (If possible) even after intentionally triggering a panic, or exiting in a way close to a crash other than `kill -TERM`, confirm no flag is left in the shell
- [ ] In every case above, confirm `Shift+Enter`/uppercase keys behave normally after restarting `ozzel` (confirming the previous session's exit handling does not adversely affect this launch)

### Regression check for this change (key layout changes)

- [ ] The uppercase keys `C`/`M`/`D`/`R`/`K` all still work as before (copy confirm, move confirm, delete confirm, rename, mkdir)
- [ ] `d`/`r` (the new lowercase aliases) also trigger delete confirmation and rename respectively
- [ ] `h`/`?` open the help screen, and `H` (Shift+h) opens the history list (`h` alone does not open history)
- [ ] `~` moves to home (`H` does not move)
- [ ] After TUI suspend/resume via `:vim`, confirm all of the above (uppercase/lowercase keys, help, history, home) still work without breaking

## 10. Config Overrides

- [ ] Launching with no config file present works with the default key bindings
- [ ] Place valid TOML at the config file path (OS-specific, see README) and confirm key reassignment via `[keys]` takes effect (e.g. `"C-c" = "copy"`, `"q" = "none"`)
- [ ] Confirm a key assigned `"none"` is actually disabled
- [ ] Launching with syntactically invalid TOML in place exits with a clear error message (without displaying the screen at all)
- [ ] Set `delete_behavior = "permanent"` and confirm deletion is permanent, bypassing the trash
- [ ] Set `home` and confirm `~` moves to that directory (also confirm `H` does not respond to home — it's dedicated to history)
- [ ] Set `editor` and confirm `e` launches with that command
- [ ] Set `confirm_operations = false` and confirm conflict-free copies/moves run immediately without confirmation (if already confirmed in section 3, a re-check here is enough)
- [ ] Set `confirm_quit = false` and confirm `q` quits immediately without confirmation when no task is running (if already confirmed in section 3, a re-check here is enough)
- [ ] Set an array such as `rename = ["r", "S-r"]` in `[bindings]` and confirm both keys actually trigger rename
- [ ] Confirm that when both `[bindings]` and `[keys]` refer to the same key, `[bindings]` wins (e.g. with `"z" = "quit"` in `[keys]` and `copy = ["z"]` in `[bindings]`, `z` becomes copy)
- [ ] Specifying a nonexistent action name or invalid key notation in `[bindings]` exits with an error message, the same as other config errors
- [ ] Set `cursor` in `[colors]` to a named color (e.g. `"red"`) and confirm the cursor color changes
- [ ] Set `cursor` in `[colors]` to a `"#RRGGBB"` hex value and confirm that color is applied
- [ ] Setting `cursor` in `[colors]` to an invalid value (e.g. `"not-a-color"`) exits with an error message, the same as other config errors
- [ ] Set `cursor_inactive` in `[colors]` to a named color / `#RRGGBB` and confirm the inactive pane's cursor color changes
- [ ] Set `dim_inactive = false` in `[colors]` and confirm the inactive pane is no longer dimmed and displays at the same brightness as the active side
- [ ] Re-confirm that key reassignment of the default bindings via `[keys]` still works after this change (e.g. `"C-c" = "copy"`, `"q" = "none"`, and that `"S-enter" = "none"` still disables it without issue)

## 11. Directory History (Back/Forward) & `--cwd-file` (new)

- [ ] After moving through a few directories, pressing `Shift+←` goes back to the previous directory, for that pane only
- [ ] After going back, pressing `Shift+→` goes forward to the directory you were at before going back
- [ ] Pressing either key when there is nowhere to go back/forward to does not error; the log shows that there's nowhere to go, and the current location does not change
- [ ] After moving the left and right panes to different directories, confirm `Shift+←`/`Shift+→` work independently for each (one pane's history does not affect the other)
- [ ] After going back with `Shift+←`, moving to a different directory (without pressing `Shift+→`) clears the "forward" stack, so `Shift+→` stops working (same behavior as a browser)
- [ ] Confirm `H` (Shift+h, the history menu) continues to work independently of this, and is not confused with `Shift+←`/`Shift+→`
- [ ] Add the README's `oz()` shell function (zsh/bash) to `~/.zshrc` or similar, launch the filer with `oz`, move to a directory, and quit with `q`. Confirm the shell's current directory follows to that directory
- [ ] Manually specify `--cwd-file <path>`, launching e.g. `cargo run -- --cwd-file /tmp/ozzel-cwd`, and confirm that after quitting, the contents of `/tmp/ozzel-cwd` match the directory of the pane that was focused at exit
- [ ] With `quit_cd = false` set in the config, launch and quit with `--cwd-file` specified, and confirm the file is not written (or its contents don't change)
- [ ] Launching and quitting normally without specifying `--cwd-file` does not error and nothing is written
- [ ] Without defining the `oz()` wrapper, launch plain `ozzel --cwd-file <path>` directly from the shell and quit. Confirm `<path>` is written to (the app itself behaves as specified), but **the shell's own current directory does not change** (confirming that without the wrapper, no `cd` happens — this is by design)

## 12. Log Viewer (`L`, new)

- [ ] Pressing `L` (Shift+l) opens the full-screen log viewer (the panels and status bar temporarily disappear)
- [ ] Right after opening, confirm the scroll position is at the latest (bottom) content
- [ ] Accumulate 500+ lines of log by performing repeated operations, open it with `L`, and confirm scrolling works with `↑`/`↓`/`PageUp`/`PageDown`/`Home` (`g`)/`End` (`G`). Confirm `Home`/`g` reaches the top (the oldest content) and `End`/`G` reaches the bottom (the latest)
- [ ] Confirm long log lines (e.g. errors containing paths) wrap, with continuation lines indented by the width of the timestamp (the same formatting as the mini-log below the status area)
- [ ] Confirm `q`/`Esc` closes the log viewer and correctly returns to the filer screen (no display corruption)
- [ ] Confirm the log viewer's contents are empty after restarting `ozzel` (in-memory only, not persisted)

## 13. Command Palette (`F`, new)

- [ ] Pressing `F` (Shift+f) opens the command palette popup in the center of the screen
- [ ] With no input, confirm all actions are listed
- [ ] Typing e.g. `rena` narrows the list to just `rename` (partial match on the action name)
- [ ] Confirm that searching for a word not contained in the action name but only in its description (e.g. `cursor`) also narrows to the matching action (confirming the description text is also searched)
- [ ] Confirm `↑`/`↓` move the highlight
- [ ] Selecting an action that requires a prompt, such as `mkdir`, and pressing `Enter` closes the palette and opens the mkdir prompt (confirming prompts/confirmation dialogs work normally even via the palette)
- [ ] Confirm `Esc` closes the palette without executing anything
- [ ] Confirm `Backspace` deletes characters from the filter string, and the list is recomputed accordingly

## 14. Row Coloring & Permissions Column (new)

- [ ] Confirm directory rows are shown in cyan (excluding the cursor row)
- [ ] Confirm hidden files and hidden directories are shown in red
- [ ] Confirm executable files (`chmod +x`'d files) are shown in yellow. Confirm a directory's execute bit (the "searchable" attribute) is ignored, and directories stay in their directory color (cyan) regardless
- [ ] Confirm a hidden directory (e.g. `.git`) is shown in red (hidden takes priority) rather than cyan (priority order: hidden > executable > directory)
- [ ] Confirm a marked row that is an executable file stays in the mark color (yellow), and check whether it's hard to visually distinguish from executable-file yellow (both being yellow means they blend together — report if this is an issue)
- [ ] In the inactive pane, confirm the type-based colors (cyan, red, yellow) appear combined with the dimming (colors dim rather than disappearing entirely)
- [ ] Change `directory`/`hidden`/`executable` in `[colors]` to different colors and confirm each is applied
- [ ] By default (`show_permissions = true`), confirm a permissions column like `drwxr-xr-x` is shown next to the name on each row. Confirm it matches `ls -l`'s output for the same file
- [ ] Confirm a symbolic link's permissions column starts with `l`
- [ ] Set `show_permissions = false` and confirm the permissions column is hidden and the name column widens accordingly
- [ ] Narrow the terminal width significantly and confirm the permissions column is automatically hidden before the name column gets crushed too much (even while `show_permissions = true`)

## 15. Mouse Operations (new)

This item is not covered by automated tests. Be sure to check it in a mouse-capable terminal (`tmux` recommended).

- [ ] Clicking an entry row in the left pane moves focus to that pane and moves the cursor to that entry (same for the right pane)
- [ ] Clicking a pane's header (the title part of the border) or empty space with no entry only moves focus, without changing the cursor position
- [ ] Confirm scrolling the wheel over the inactive pane does not move focus, and only moves that pane's cursor by 3 lines at a time (the active side remains unchanged)
- [ ] Confirm scrolling the wheel over the active pane likewise moves the cursor by 3 lines at a time
- [ ] Dragging with the left button from an entry row marks the rows in the range from the drag start position (the anchor) to the current position (unmarked rows get marked)
- [ ] Dragging over an already-marked range unmarks those rows (deselection via drag; the mark state at drag start is the baseline)
- [ ] After extending the range and then pulling the pointer back to shrink it (retreating), confirm rows that fall out of the range automatically revert to their "before drag started" state (this is a live, rubber-band selection where toggling isn't fixed once applied — it only stays toggled while inside the range)
- [ ] Dragging in the reverse direction past the anchor (overshooting to the other side and back) causes rows that exit the range to revert, while newly entered rows get toggled
- [ ] Dragging over a row that was already marked before the drag started causes it to unmark temporarily, then return to marked once it leaves the range
- [ ] Starting a drag in one pane and moving the pointer into the other pane does not toggle anything there, nor does focus move (the drag stays locked to the pane it started in)
- [ ] After releasing the drag (mouse up), confirm normal clicks/key operations work fine again
- [ ] Double-clicking a directory row moves into that directory (same behavior as `open`)
- [ ] Double-clicking a file row opens the built-in viewer
- [ ] Confirm mouse capture is automatically released when opening the viewer, log viewer, or help screen (with `mouse = true`, clicking normally moves the cursor in the filer screen, but clicks/drags stop reaching ozzel on these three screens)
- [ ] In the viewer, drag-select a range spanning multiple lines with the mouse, then copy it via the OS (the terminal's menu or shortcut) and paste into another app. Confirm only the selected body text is included, with no border characters, title, or footer line mixed in (paste-check). Confirm the same for the log viewer and help screen
- [ ] Visually confirm that none of the viewer, log viewer, or help screen show any border lines anywhere on screen (the title/scroll position only appear on the footer line)
- [ ] Confirm scrolling the wheel while the viewer, log viewer, or help screen is displayed has no effect on ozzel (no scrolling), and instead becomes the terminal's native scrollback. Confirm keyboard scrolling (`↑`/`↓`/`PageUp`/`PageDown`/`Home`/`End`, etc.) still works
- [ ] Closing the viewer, log viewer, or help screen with `q`/`Esc` and returning to the filer automatically resumes mouse capture, and clicking to move the cursor / dragging to mark works again
- [ ] Confirm mouse capture is not released (stays active) while the command palette (`F`) is open
- [ ] Confirm opening the viewer, log viewer, or help screen with `mouse = false` causes no particular issue (since capture was already disabled)
- [ ] Confirm mouse operations are ignored during modals other than the viewer/log/help — prompts, confirmation dialogs, bookmark/history menus, etc. — and clicking does not close them (only key operations such as `Esc` close them)
- [ ] Confirm mouse operations (clicking, wheel) keep working after suspending/resuming the TUI via e.g. `: vim <file>` (also confirm the mouse works normally within vim during the suspension)
- [ ] Launch with `mouse = false` and confirm clicking, dragging, and the wheel have no effect on ozzel, and the terminal's native text selection (drag-select/copy) works instead
- [ ] With `mouse = true` (default), confirm holding Shift while dragging performs the terminal's native text selection, bypassing ozzel (behavior may vary by environment — report if so)

## 16. Duplicate & Path-Copy (`c`/`y`, new)

- [ ] Pressing `c` on a file opens a prompt for the duplicate's name, pre-filled with the current name
- [ ] Entering a different name and confirming creates a copy with that name in the same directory (shown in the log/gauge as an async task)
- [ ] Confirming without changing anything (same name) shows an error in the log, and no duplicate is made
- [ ] Entering a name that already exists shows an error in the log, and nothing is overwritten
- [ ] Pressing `c` on a directory recursively duplicates it, including nested directories
- [ ] Pressing `y` logs something like "copied: /path/to/file"
- [ ] On an OSC 52-capable terminal (tmux with supporting config, or a supporting terminal alone), press `y` and confirm the file's absolute path actually ends up in the system clipboard (verify by pasting with `Cmd+V`/`Ctrl+V` into another app)

## 17. Two-Line Wrapping of Pane Headers (new)

- [ ] Move to a deeply nested, long directory path and confirm that when the border title fits on one line, it stays on one line
- [ ] Narrow the terminal width so the path no longer fits on one line, and confirm the header automatically wraps to two lines, reducing the entry list's displayed row count by one
- [ ] Confirm two-line wrapping also works correctly when the header is long while filtering is active (with the `[flt: ...]` tag)
- [ ] Confirm that when the width is narrowed so extremely that even two lines don't fit, the left side is truncated with `…`
- [ ] Confirm full-width characters are not split in half when a header with a Japanese directory name wraps to two lines

## 18. Virtual Directory (Browsing a zip Like a Directory, new)

- [ ] Prepare a `.zip` containing nested directories and multiple files, put the cursor on it, and press `Enter`/`o`. Confirm the archive's contents are listed in place — not opened in the viewer (the pane header becomes `archive.zip:/`)
- [ ] Confirm you can descend into a subdirectory inside the archive with `Enter`, and the header updates to something like `archive.zip:/subdirectory-name`
- [ ] Even if the archive-creation tool did not write explicit directory entries (only file-entry paths), confirm intermediate directories are correctly synthesized and shown in the listing
- [ ] Confirm `Backspace` goes up one level within the archive
- [ ] Pressing `Backspace` at the archive root exits the Virtual Directory back to the real directory, with the cursor restored to the position of the original `.zip` file
- [ ] Pressing `Enter`/`o` on a text file inside the archive opens the built-in viewer in place (without extracting to disk), showing the correct content (the footer shows a label in `archive.zip:/path` format)
- [ ] Mark entries inside the archive (`Space`) and press `C` (copy). Confirm a "Extract N item(s) -> /dest? (y/n)" confirmation appears, and `y` asynchronously extracts them to the other pane's real directory (including progress gauge display)
- [ ] Marking a directory and pressing `C` recreates that whole subtree at the destination (as a directory with its own name)
- [ ] Confirm that pressing any of `M` (move), `D` (delete), `R`/`r` (rename), `K` (mkdir), `c` (duplicate), `p` (zip), `u` (unzip), `e` (open in editor), or `Shift+Enter` (open with OS default app) inside the archive is not executed, and logs a "read-only" style error
- [ ] Confirm `f`/`/` (filter) and `s` (sort toggle) work fine inside the archive
- [ ] Confirm that pressing `:` inside the archive opens the command line prompt with its current directory set to the real directory containing the archive (verify with e.g. `pwd`)
- [ ] Moving from inside the archive to another real directory via `~` (home) or the bookmark/history menu exits the Virtual Directory and shows that real directory
- [ ] With an archive open, confirm normal file operations (copy, delete, etc.) work fine in the other pane (both panes operate independently)
- [ ] Attempting to open a nonexistent/corrupt zip, or a password-protected zip, does not crash and logs an error

## 19. Per-Extension External Viewers (`[viewers]`, new)

- [ ] Add a `[viewers]` section to the config with an entry like `md = "less {}"`. Pressing `Enter`/`o` on a real file with that extension launches `less` instead of the built-in viewer, and it displays and quits normally (after quitting, the display is not corrupted and returns to the filer)
- [ ] Confirm that even a command without `{}` (e.g. `md = "less"`) launches correctly, with the path automatically appended at the end
- [ ] Confirm files with extensions that have no corresponding `[viewers]` entry still open in the built-in viewer as before
- [ ] Confirm files with no extension always open in the built-in viewer, regardless of what's set in `[viewers]`
- [ ] For a file inside a Virtual Directory (zip) that has a matching `[viewers]` entry, confirm the external viewer does not launch — it falls back to the built-in viewer, and the log shows a message to the effect that external viewers can't be used inside archives
- [ ] Confirm `Shift+Enter` (`open_default`, open with the OS default app) is unaffected by `[viewers]` and always opens with the OS default app
- [ ] Edit the config file with `,` to add/change a `[viewers]` entry and save. Confirm the new setting takes effect immediately without restarting the app (after the "config reloaded" log, open a file with the target extension to check)

## 20. Prefix Jump (`\`, new)

- [ ] Pressing `\` enters jump mode, showing a `Jump: ` input field at the bottom
- [ ] Confirm that as each character is typed, the cursor moves to the first entry matching the typed string (prefix match, case-insensitive). **Confirm the list itself is not filtered — no entries become hidden** (this is the difference from the `f`/`/` filter)
- [ ] Confirm prefix jump also works when typing Japanese characters for a Japanese file name
- [ ] Confirm that as you type more characters, the candidates narrow and the cursor is recomputed each time (e.g. `a` matches several → `ab` narrows further)
- [ ] With multiple matching entries, pressing `Down` (or `Tab`) moves the cursor to the next entry matching the same input. Confirm that pressing `Down` again past the last match wraps around to the first match
- [ ] Pressing `Up` moves to the previous match. Confirm pressing `Up` at the first match wraps around to the last match
- [ ] If no entry matches the input, confirm the cursor doesn't move and the input field shows a warning such as `(no match)`
- [ ] Confirm that exiting jump mode with `Esc` restores the cursor to the position it was at when the mode started
- [ ] Confirm that exiting jump mode with `Enter` keeps the cursor at its current position as moved by the search
- [ ] Confirm the `..` (go-to-parent) row never matches any input
- [ ] Confirm a `jump_search` row is shown in the "Filtering" category of the help screen (`h`/`?`)
- [ ] Confirm prefix jump with `\` also works fine in a Virtual Directory (browsing inside a `.zip`)
- [ ] Confirm `examples/config.toml`'s `[bindings]` section includes `jump_search = ["\\"]`, and that `cargo test`'s `[bindings]` generation drift-detection test passes (as a development-time checklist item)

## 21. `update` Subcommand (new)

- [ ] Running `ozzel update` first shows the current version, e.g. `current version: 0.1.0`
- [ ] Since the repository is not yet public on GitHub at this stage, confirm it then shows something like "could not determine the remote version. Reinstalling anyway.", after which it attempts to run `cargo install --git ...`, which fails because the repository can't be found (no crash, no panic)
- [ ] Confirm `ozzel update --force` behaves the same way, and that `--force` only changes behavior when the remote version matches the current one (in which case only it forces a reinstall) (since the remote check itself currently fails, there should be little visible difference with/without `--force`, but confirm there is no crash)
- [ ] If you can assume `cargo` is not present (or temporarily remove it from `PATH`) and run it, confirm a clear error to the effect that cargo could not be run is shown (can be skipped depending on environment)
- [ ] Confirm `ozzel` (no arguments) still launches the TUI as before (confirming normal launch is not broken by adding the `update` subcommand)
- [ ] Confirm normal launches with two positional arguments, like `ozzel . ..`, still work as before
- [ ] Confirm launching with `ozzel --cwd-file <path>` and no positional arguments still works as before
- [ ] Actually create a directory named `update` in the current directory and run `ozzel update`; confirm it's interpreted as the update subcommand (rather than opening the directory). Confirm you can still explicitly open that same directory with `ozzel ./update`
- [ ] Confirm `ozzel --help`'s output lists the `update` subcommand, and `ozzel update --help` shows a description of the `--force` flag

## 22. `less`-Compatible Search in the Viewer (`/`, `?`, `n`, `N`, new)

- [ ] Open a fairly large Rust source file or similar in the viewer (one with several matches for some word makes it easier to check), press `/`, and confirm an input field opens at the bottom of the screen (same look as the `f`/filter input field)
- [ ] Type a search term and press `Enter`; confirm it jumps to the first line matching at or after the current top row
- [ ] If multiple matches are visible on screen, confirm each is highlighted in reverse video
- [ ] Confirm the footer shows the search string and `current-position/match-count` (e.g. `/needle  3/17`)
- [ ] Confirm pressing `n` repeatedly moves to the next match each time, wrapping to the first match after reaching the end of the file, and that the footer shows `(search wrapped)` on the wrap-around
- [ ] Confirm pressing `N` (Shift+n) moves in the opposite direction from `n`
- [ ] Confirm `?` opens a backward-search input field (prefixed with `?`), and `Enter` jumps to a match at or before the current top row. Confirm that for a search started with `?`, `n` moves upward and `N` moves downward
- [ ] Confirm that searching with a string that is invalid as a regex (e.g. an unclosed parenthesis like `foo(bar`) is treated as a plain substring search, and does not crash
- [ ] Confirm that a valid regex (e.g. `TODO|FIXME`) highlights all matches for either pattern together
- [ ] Confirm that searching for a string with no matches leaves the cursor/scroll position unchanged and logs an error message
- [ ] Confirm pressing `Esc` while typing a search cancels the input and restores whatever search state (and its highlighting, if any) was active before
- [ ] With an active search (not currently typing), confirm pressing `Esc` clears only the highlighting and the viewer stays open. Confirm pressing `Esc` again closes the viewer and returns to the filer
- [ ] Switch to hex dump display with `Tab`, then search for a hex value (e.g. `48 65`) with `/`, and confirm it jumps to the line containing the corresponding bytes
- [ ] Confirm you can search with a Japanese search term against a line containing Japanese text, and the correct range is highlighted (no garbling, no truncation)
- [ ] Confirm matches that are off-screen due to horizontal scrolling (`←`/`→`) still count toward `n`/`N` navigation, and that the highlight displays correctly once you scroll back
- [ ] Confirm the help screen (`h`/`?`... though not the `?` inside the viewer) continues to work independent of this change (confirming `?` means backward search inside the viewer, but still opens help as before on the filer screen)

## 23. Symbolic Links (Treated as Directories + Safe Operations, new)

Setup (create with real `ln -s`):

```sh
mkdir real_dir && echo hi > real_dir/inside.txt
echo hello > target.txt && chmod +x target.txt
ln -s real_dir link_to_dir
ln -s target.txt link_to_file.txt
ln -s does_not_exist dangling_link
```

- [ ] Confirm the `link_to_dir` row is shown in the same color as a directory (cyan) and its size column shows `<DIR>`. Confirm the name ends with a `@` (marking it as a symbolic link)
- [ ] Confirm `link_to_dir` is grouped with real directories like `real_dir` in the sort order too (this doesn't break even when toggling the sort key with `s`)
- [ ] Put the cursor on `link_to_dir` and press `Enter` (or `o`) to enter it, confirming `inside.txt` is shown (the listing works correctly because `fs::read_dir` automatically follows the link)
- [ ] Right after entering `link_to_dir`, confirm the pane header (current directory display) shows `.../link_to_dir` (not normalized to `real_dir`)
- [ ] In that state, pressing `Backspace` confirms it returns to the original directory that contained `link_to_dir`, with the cursor restored onto `link_to_dir` itself
- [ ] Put the cursor on `link_to_file.txt` and press `Enter` (or `o`); confirm the built-in viewer opens showing `target.txt`'s content (`hello`)
- [ ] With `target.txt` made executable (`chmod +x`), confirm the `link_to_file.txt` row is shown in the executable-file color (yellow) (confirming it's judged by the link target's mode, not the link's own permissions)
- [ ] Put the cursor on `dangling_link` (a broken link) and press `Enter`; confirm neither navigation nor the viewer opens, and an error such as `No such file or directory` is logged (no crash)
- [ ] Confirm the `dangling_link` row is not shown in the directory color (and does not show `<DIR>`), looking like a normal file
- [ ] Copy `link_to_dir` to the other pane with `C` (copy) and confirm the destination is also created as a **symbolic link** (check with `ls -la`; the destination must not become a directory tree with real content)
- [ ] Delete `link_to_dir` with `D` (delete) and confirm only the link itself is removed, while `real_dir` (the link target) and its contents (`inside.txt`) remain intact
- [ ] Duplicate `link_to_dir` with `c` (duplicate) and confirm the duplicate is also created as a symbolic link, not a real copy
- [ ] Confirm `\` (prefix jump), `f` (filter), and `Space` (mark) all work normally on `link_to_dir`/`link_to_file.txt` just like regular entries
- [ ] With a `[viewers]` setting such as `md = "less {}"`, open `link_to_notes` (a link to `notes.md`) which has no extension itself, and confirm it falls back to the link target's extension `md`, launching `less` (can be skipped if unavailable)

## 24. Virtual Directory: tar-Family Archives (`.tar`/`.tar.gz`/`.tgz`/`.tar.bz2`/`.tbz2`/`.tar.xz`/`.txz`, new)

Setup (create with the system's real `tar` — ozzel does not support **creating** tar archives, so only check the extraction/browsing side):

```sh
mkdir -p project/src/nested
echo hello > project/readme.txt
echo 'fn main() {}' > project/src/main.rs
echo deep > project/src/nested/deep.txt
tar -cf project.tar project
tar -czf project.tar.gz project
cp project.tar.gz project.tgz
tar -cjf project.tar.bz2 project   # bzip2 (skip if bzip2/tar aren't available)
tar -cJf project.tar.xz project    # xz (skip if xz/tar aren't available)
```

- [ ] Pressing `Enter`/`o` on `project.tar` browses its contents (under `project/`) in place, like a directory, without extracting
- [ ] Confirm both `project.tar.gz` and `project.tgz` (same content, different extension) open the same way (confirming recognition of the `.tgz` suffix)
- [ ] Confirm `project.tar.bz2`/`project.tbz2` can be opened (skip if unavailable)
- [ ] Confirm `project.tar.xz`/`project.txz` can be opened (skip if unavailable)
- [ ] For every format, confirm intermediate directories like `src/` are automatically synthesized and shown, even without an explicit directory entry on the tar side
- [ ] Confirm `Backspace` goes up one level at a time inside the archive, and pressing it at the root exits the archive back to the real directory, with the cursor restored to the position of the original archive file
- [ ] Pressing `Enter`/`o` on a file inside the archive (e.g. `readme.txt`) opens its content in the built-in viewer
- [ ] Confirm the pane header shows `archive-name:internal-path` format, e.g. `project.tar.gz:/src`
- [ ] Confirm that trying `M` (move), `D` (delete), `R`/`r` (rename), `K` (mkdir), `c` (duplicate), `p` (zip), `e` (editor), or `Shift+Enter` (OS default app) inside the archive all result in a logged error and are not executed (read-only)
- [ ] Mark entries (`Space`) and press `C`; confirm they extract to the real directory in the other pane (check both a single file and a subtree, and confirm the extracted content matches the original)
- [ ] Open a fairly large `.tar.gz` (tens of MB to ~100MB, if available) and confirm the listing may take a noticeable amount of time (no crash; confirming the README's note that tar formats are read sequentially and can be slow — skip if unavailable)
- [ ] Confirm the `u` (unzip) key on a `.tar`/`.tar.gz` etc. results in an error like "selected entry is not a .zip file", confirming bulk extraction of tar-family archives is not supported (`u` remains zip-only)
- [ ] Confirm that a plain `.gz` file (a standalone gzip file, not `.tar.gz`), or a `.tar.*` file using an unsupported compression method (zstd, etc.), is not recognized as a Virtual Directory and opens as a normal file in the built-in viewer (hex dump display, since it's binary)

## 25. Full Logging of Operation Target Paths (Copy, Move, Delete, Duplicate, Zip, Unzip, Extract, new)

- [ ] Mark 3 files and run `C` (copy). Confirm that at the moment execution starts, the log records one line per target file (3 lines) in the format `copy: /absolute/path/source -> /absolute/path/dest` (opening the log viewer with `L` is a good way to check)
- [ ] Similarly, confirm `M` (move) logs each one in `move: ... -> ...` format
- [ ] Mark 3 files and run `D` (delete); confirm it logs one line per target file in `delete: /absolute/path` format
- [ ] Running `c` (duplicate) logs `duplicate: /absolute/path/source -> /absolute/path/dest`
- [ ] Compressing several marked files with `p` (zip) logs `zip: /absolute/path` once per target
- [ ] Running `u` (unzip) logs `unzip: /absolute/path/archive.zip -> /absolute/path/destination`
- [ ] Marking entries inside a Virtual Directory and running `C` (extract) logs `extract: archive-name:/internal-path -> /absolute/path/destination` format
- [ ] Mark a large number (10+) of files and delete/copy them; confirm the log gets that many lines added at once without crashing. Confirm the normal status bar display is not disrupted, and that opening the log viewer with `L` lets you scroll through and check every entry

## 26. Centered Popup Display for Prompts (new)

- [ ] Pressing `R`/`r` (rename) shows a popup box centered on screen, rather than a single line at the bottom, with the title "Rename" and the input field pre-filled with the current name
- [ ] Confirm `K` (mkdir) similarly shows a centered popup titled "New directory"
- [ ] Confirm `c` (duplicate) similarly shows a centered popup titled "Duplicate as", with the input field pre-filled with the current name
- [ ] Confirm `p` (zip) similarly shows a centered popup titled "Zip as"
- [ ] Confirm `:` (command line) similarly shows a centered popup titled "Command"
- [ ] Confirm every popup shows a hint line at the bottom reading `Enter: OK   Esc: Cancel`
- [ ] Confirm the normal status bar below (e.g. the current directory display) remains visible while a popup is shown (aside from the part hidden behind the popup)
- [ ] Typing a string longer than the popup's input field scrolls it horizontally according to the cursor position, keeping the text being typed visible
- [ ] Confirm `Enter` to confirm / `Esc` to cancel still work correctly as before (canceling creates/changes nothing)
- [ ] On the other hand, confirm `f`/`/` (filter), `\` (prefix jump), and the viewer's `/`/`?` (search) still display as a single line at the **bottom** of the screen, not as a centered popup (so the list/body text remains visible without being hidden while typing)

## 27. Settings Screen (`S`, new)

- [ ] Pressing `S` (Shift+s) opens the settings screen, showing a category list (Behavior, Colors, Startup/Integration, Extension Viewers, Key Bindings). Confirm searching `settings` in the command palette (`F`) opens the same screen
- [ ] Select "Behavior" and press `Enter` on the `mouse` row; confirm `ON`/`OFF` toggles immediately, and that `mouse = false` (or `true`) is written to the config file (the one openable with `,`). Confirm the setting takes effect right after writing (e.g. right after setting `mouse = false`, clicking no longer moves the cursor)
- [ ] Press `Enter` on the `delete_behavior` row; confirm a two-choice list of `trash`/`permanent` appears, and `↓`→`Enter` switches it to `permanent`, which is also reflected in the config file
- [ ] Confirm `Esc` steps back one level at a time: item list → category list → filer
- [ ] In the "Colors" category, select `directory`, choose `magenta` from the named color palette (with swatches), and press `Enter`; confirm that right after closing the settings screen, the filer's directory rows have actually changed color (**the key point here is that the color change takes effect immediately on screen**)
- [ ] For the same color item, select "custom hex" at the end of the list, enter a 6-digit hex value like `112233`, and press `Enter`; confirm it's saved and applied as `#112233`
- [ ] In the "Startup/Integration" category, enter a value for `editor` and confirm with `Enter`; confirm the editor that opens with `,` (edit config) actually changes. Confirm that deleting the value entirely and confirming with it empty reverts to "unset" (falling back to `$EDITOR`)
- [ ] In the "Extension Viewers" category, use "+ add new" to add extension `md` and command `glow {}` (use `Tab` to switch between the extension field and command field), then confirm opening a `.md` file actually opens it via `glow`. Confirm selecting the same entry and pressing `d` removes it from the list, after which `.md` opens in the built-in viewer again
- [ ] In the "Key Bindings" category, select any action (e.g. `mkdir`), press `a`, and press an unused key (e.g. `z`); confirm a capture confirmation screen appears (`Bind "z" to mkdir?`), and confirm that `y`/`Enter` applies it, making that key actually trigger mkdir
- [ ] Similarly, capturing a key already used by another action (e.g. `r`, bound to `rename` by default) shows a confirmation screen with a warning about taking it away from that action. Confirm that `y`/`Enter` reassigns the key to the new action, removing it from the original action (`rename`) (confirm nothing changes if canceled with `n`/`Esc`)
- [ ] After the key binding change above, check the config file and confirm the key taken away is recorded as `"none"` in `[keys]`, and the one it was assigned to is added to `[bindings]`
- [ ] In the key bindings list, pressing `d` deletes the combo at the cursor position, and confirm that key no longer works
- [ ] **Comment preservation check**: write a manual comment (`# ...`) into the config file beforehand, change just one item from the settings screen, then open the file directly and confirm the original comment is still fully intact (confirming `toml_edit`-based diff application isn't broken)
- [ ] Confirm the settings screen is full-screen, with the panes, log area, and status bar not visible while it's open

## 28. `less`-Compatible Scroll/Search in the Help Screen & Log Viewer (new)

- [ ] Open the help screen with `h` (or `?`) and confirm you can move 1 line at a time with `j`/`k`, page forward with `Space`/`f`/`PageDown`, page back with `b`/`PageUp`, half-page forward/back with `d`/`u`, and jump to the top/bottom with `g`/`Home` and `G`/`End`
- [ ] Pressing `/` in the help screen opens a search input field at the bottom; typing a known action name like `rename` and pressing `Enter` jumps to that key-binding row and highlights it in reverse video
- [ ] Confirm `n`/`N` move to the next/previous match (if there's only one match, confirm pressing it again shows `(search wrapped)` in the footer, as a "wrap" to the same line)
- [ ] With an active search in the help screen, confirm pressing `Esc` clears only the highlight, leaving the screen open. Confirm pressing `Esc` again (or `q`/`h`) closes the help screen
- [ ] Open the log viewer with `L` (Shift+l) and confirm `j`/`k`/`Space`/`f`/`b`/`d`/`u`/`g`/`G` all work the same way (note that `↑`/`k` moves away from the latest side (downward in log order) and `↓`/`j` moves toward the latest side — the up/down meaning here is reversed compared to the viewer/help screen)
- [ ] With a log entry containing Japanese text present (e.g. a log from an operation involving a Japanese file name, or from creating a file with a Japanese name), open the log viewer, search for that Japanese string with `/`, and confirm it's correctly matched and highlighted (**Japanese search hit** check)
- [ ] With an active search in the log viewer, confirm pressing `Esc` clears only the highlight, and pressing `Esc` again (or `q`) closes it
- [ ] Regression check: confirm the text viewer's (`o`/`Enter`) `/`, `?`, `n`, `N` search still works correctly as before (including highlighting, wrap notification, and the footer position display)

## 29. `~` Expansion in the `home` Setting (new)

- [ ] Set `home` in the config file to `~/work` (an existing directory under your actual home directory) and launch `ozzel`; confirm pressing `~` (`GoHome`) actually moves there (previously this was a bug that produced an error like `not a directory: ~/work`)
- [ ] Even when the directory specified in `home` is a symbolic link (e.g. `~/work` is actually a link pointing elsewhere), confirm it correctly moves to the real target directory
- [ ] Confirm that specifying `home = "~"` (`~` alone) moves to the OS home directory itself
- [ ] Confirm that specifying an absolute path in `home` (e.g. `/tmp/somewhere`) is used as-is, unchanged, as before

## 30. File Name Search (`g`, new)

- [ ] In a directory with subdirectories, pressing `g` opens a centered popup listing all entries underneath (both files and directories; directories have a trailing `/`) as paths relative to the root
- [ ] Confirm results narrow incrementally as each character is typed (default setting), and the title's hit count updates accordingly
- [ ] Confirm it's a case-insensitive partial match (e.g. `readme` matches `README.md`)
- [ ] Confirm starting with `re:` switches to a case-sensitive regular expression (e.g. `re:\.rs$`)
- [ ] Confirm entering an invalid regex (e.g. `re:[`) shows a red error message below the input field, without crashing, and yields 0 results. Confirm deleting one character back to a valid pattern makes the error disappear
- [ ] Confirm `Up`/`Down` move the cursor in the results list, and `Enter` moves to that entry's parent directory with the cursor placed on that entry (check with a deeply nested file)
- [ ] Confirm that selecting a directory and pressing `Enter` likewise moves to its parent directory with the cursor placed on that directory (and that pressing `Enter` again enters it)
- [ ] Confirm `Esc` closes the popup without moving the pane's displayed directory or cursor at all
- [ ] With hidden files not shown (default), confirm dotfiles and contents under hidden directories don't appear in results, and confirm they do appear if you toggle display with `.` and press `g` again
- [ ] In the settings screen (`S` → Behavior) or the config, set `file_search_incremental = false`; confirm results don't update while typing (the title shows `[Enter to search]`), the first `Enter` runs the search, and the second `Enter` navigates
- [ ] Confirm pressing `g` while inside a Virtual Directory (browsing inside a `.zip`) does not open the popup, and logs a read-only error instead
- [ ] Confirm the title shows `[truncated]` when opened on a huge tree (e.g. over 100,000 entries, such as directly under the home directory)

## 31. Interactive Shell Mode for the `:` Command (`command_line_interactive`, new)

- [ ] By default (`command_line_interactive = false`), confirm running an alias defined in `.zshrc` via `:` results in "command not found" (the same behavior as before)
- [ ] With `command_line_interactive = true` (via config or the settings screen `S` → Behavior), confirm running the same alias succeeds. Confirm the same for a shell function defined in `.zshrc`
- [ ] With it `true`, confirm normal commands (e.g. `ls -la`) still run as before, and the "press any key" wait → return to the filer after completion is unchanged
- [ ] With it `true`, confirm `e` (editor) and `[viewers]` commands still work as before (unaffected by this setting)
- [ ] Confirm `ozzel` itself survives if you press Ctrl+C on a running child process (a regression check around signal handling in interactive mode)

## 32. Regression Check (Overall)


- [ ] Confirm `open` (`Enter`/`o`), copy (`C`), move (`M`), delete (`D`), filter (`f`), the viewer (`o`), `:vim` suspend/resume, and live reload of config via `,` all still work without issue after this change
- [ ] Confirm `cargo build` / `cargo clippy --all-targets -- -D warnings` / `cargo fmt --check` / `cargo test` are all clean (as a development-time checklist item)
