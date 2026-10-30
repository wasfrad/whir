# whir

A fast, lean, native Windows app for the files that matter at work: **images, PDFs,
CSVs and notes**. Double-click a file and it's on screen; open another and it becomes a
tab in the same window. Start whir on its own and you get a blank note to type into —
it saves itself, no questions asked. A small capture dock takes screenshots, keeps them,
and opens each one ready to highlight and label.

- Rust + raw Win32 + Direct3D 11 + egui (no eframe/winit/wgpu, no web runtime)
- One portable folder: `whir.exe` (~5.5 MB, static CRT) + `pdfium.dll` (~7 MB, loaded only when a PDF is opened)
- No installer, no admin rights, no VC++ redistributable

## Using it on a work PC

1. Build (below) or copy `dist\whir\` anywhere you can write (e.g. `%LOCALAPPDATA%\Programs\whir`).
2. Run once: `whir.exe --register`
   This adds per-user "Open with" entries for images, PDFs, CSVs, Markdown and text files (HKCU only).
3. Right-click a `.png`, `.pdf` and `.csv` → **Open with → Choose another app → whir → Always**.
   (Windows doesn't let programs make themselves the default; this click is required once per type.)

`whir.exe --unregister` removes the entries and stops whir starting with Windows.
Deleting the folder removes the program (quit whir from its tray icon first: the dock
is `whir.exe` too, and keeps running when the window is closed).

If your PC blocks unsigned programs (AppLocker/WDAC), try running `whir.exe` from the
folder first. If it's blocked there, it will be blocked everywhere.

## Keyboard & mouse

| Everywhere | |
|---|---|
| Ctrl+N · `+` | New note |
| Ctrl+Shift+N · right-click `+` | New table (CSV) |
| Ctrl+O | Open files |
| Ctrl+S · Ctrl+Shift+S | Save · Save as / export a copy |
| Ctrl+E | The drawer along the bottom: take it to the keyboard · fold it away |
| Camera button at the right of the toolbar | Open the capture dock |
| Ctrl+, · the gear next to it | Settings: the timesheet, customers & projects, PDFs & images, startup & window |
| Ctrl+Shift+F · F4 / Shift+F4 | Find in all tabs · next / previous result, in any file |
| Ctrl+W / Ctrl+F4 / middle-click tab | Close tab (closing the last one closes whir) |
| Right-click a tab | Close others · close tabs to the right · close all (leaves a blank note) · copy path · show in Explorer |
| Ctrl+Tab / Ctrl+Shift+Tab, Ctrl+PgDn / Ctrl+PgUp | Next / previous tab |
| Ctrl+1 … Ctrl+8, Ctrl+9 | Go to tab n / last tab |
| Ctrl+Shift+T | Reopen closed tab |
| Drag files onto the window | Open them |

A ● on a tab means unsaved changes; closing it (or whir) asks whether to save.

The window opens where it was left: the same size and place, maximized if it was. If
the mouse is on another monitor, it opens there instead, centred, at the same size.
**Settings › Startup & window** can keep it exactly where it was instead. It is never
left off-screen when monitors change.

| Notes | |
|---|---|
| Title field · ↓ / Enter | Name the note (it becomes the file name) · go to the text |
| Enter in a list / task / quote | Continue it (Enter on an empty item ends it) |
| Tab / Shift+Tab | Indent / outdent list items or selected lines |
| Ctrl+Enter · click a box | Toggle a `- [ ]` task |
| Ctrl+B · Ctrl+I · Ctrl+K | Bold · italic · link |
| Ctrl+click a link | Open it |
| Ctrl+Z / Ctrl+Y | Undo / redo |
| Ctrl+Shift+D · the flag in the toolbar | Flag the note as a todo · mark it done |

Notes are plain Markdown files in `Documents\whir`, saved about a second after you stop
typing (and whenever you switch away). Other `.md` / `.txt` files open in the same editor
and are saved with Ctrl+S.

A note flagged as a todo comes first in the notes drawer (⚑), the most recently flagged
first, and stays in sight on the capture dock: while any todo is open the dock stays out
with a card listing them (click one to open it in whir, ✓ to mark it done). Done ones go
back among the rest with a ✓. Flag, mark done or unflag from the toolbar, Ctrl+Shift+D,
right-clicking a note in the drawer, or F in the drawer. The flags are kept beside the
notes, in `Documents\whir\.whir\todos.txt`, so the notes stay plain Markdown; a renamed
note keeps its flag, and a deleted one takes it along.

The drawer along the bottom of the window shows what's near the open document: a note
(or a table in the notes folder) has every note, newest first, wrapped into columns; a
screenshot has the screenshots as a filmstrip, their highlights and text drawn on them.
Other images and PDFs have no drawer (Ctrl+E brings up the notes there anyway, until you
switch tabs).

| Drawer | |
|---|---|
| Click its header · drag its top edge | Fold it down to the header or unfold it · make it taller or shorter (bigger pictures, more notes); both are remembered for each kind |
| Click · right-click | Open · Show in Explorer / Delete (to the Recycle Bin) |
| Wheel over the filmstrip | Scroll it sideways |
| Ctrl+E, then ↑ ↓ ← → · Enter · Delete · Esc | Keyboard: move · open · delete · give the keyboard back |

| Images | |
|---|---|
| Wheel, Ctrl+wheel, pinch | Zoom at the cursor |
| Drag | Pan |
| Double-click | Toggle fit / 100% |
| 0 · 1 · + · − | Fit · 100% · zoom in · zoom out |
| ← → · Home End · PgUp PgDn | Previous/next image in the folder (Explorer's sort order) · first/last |
| R · Shift+R · right-click | Rotate right · left (on screen only, for as long as the tab is open; the file isn't changed) |
| H · the highlighter in the toolbar (hold or right-click it for colours) | Highlighter on / off |
| T · the text tool in the toolbar (hold or right-click it for colours) | Text on / off |
| Highlighter on: drag | Draw a highlight |
| Text on: click | Type there (Enter: new line · Esc, Ctrl+Enter or a click elsewhere: done) |
| Either on: click · drag a selected mark · drag its handles · Delete · Esc | Select a highlight or text · move it · resize it · remove it · let go (selection, then the tool) |
| Either on: double-click text | Change what it says |
| Ctrl+Z · Ctrl+Y / Ctrl+Shift+Z | Undo · redo marks |
| Ctrl+C · right-click → Copy image | Copy the image as it is on screen, marks and rotation included |
| Ctrl+Shift+S · right-click → Save a copy… | Save that as a new PNG (Ctrl+S offers the same once there are marks) |

An image opens fitted: a large one shrinks to the window, a small one stays at 100%.
**Settings › PDFs & images** can open each one at actual size instead, or at the zoom
used last on any image (← → through a folder too). The right-click menu has Fit to
window (0) and Actual size (1).

Highlights tint what is under them the way a marker does: white paper takes the colour,
black text stays black. Text is drawn with a thin halo (white around dark colours, dark
around white) so it reads on a busy screenshot; resizing it from a corner scales it.
Neither is ever drawn into the file. On a screenshot (in the screenshots folder) they are saved as you
make them and can be moved, resized, changed or removed any time; on any other image
they last as long as the tab. What leaves whir with the marks in it is a copy: the
clipboard, or Save a copy.

## Screenshots

The capture dock is a slim grip on a screen edge that slides out when the pointer rests
on it. It stays on top of every other window, full-screen ones included (only the Start
menu and Windows' notifications go over it). It is a small process of its own (about
5 MB) and is the part of whir that is always running: it starts with Windows and with
whir's window, stays when the window is closed, and has an icon in the notification
area. Quitting whir is done from that icon.

| Capture dock | |
|---|---|
| Camera · Ctrl+PrtSc from anywhere | Capture: the screen freezes and dims |
| …then drag · click a window · click the desktop | Take that region · that window · that monitor |
| Esc · right-click | Cancel |
| Stopwatch | Delay: off → 3 s → 5 s (time to open a menu or a tooltip before the freeze) |
| Drag the dock | Move it to any edge of any monitor (remembered) |
| Right-click the dock | Capture · delay · open whir · open the screenshots folder · hide the dock |
| Click the tray icon · right-click it | Open whir · capture, show/hide the dock, start with Windows on/off, **Quit whir** |

Each capture is saved as a PNG in `Documents\whir\Screenshots` and opens in whir with the
highlighter in hand; its drawer is a filmstrip of them all. Their highlights and text are
kept beside them (in `Screenshots\.whir`), so the PNG is always the capture itself.
`whir.exe --dock` shows the dock (the camera button in the toolbar does the same, even
when it is hidden); `--dock-start` starts it without showing it; `--dock-capture`
captures at once; `--dock-quit` closes it. whir starts with Windows (a per-user entry
under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, set the first time the
dock runs; switch it off in the tray menu). The
hotkey can be changed or switched off in `%LOCALAPPDATA%\whir\dock.ini`
(`hotkey = ctrl+alt+f9`, `hotkey = off`).

Captures are taken with GDI, which keeps the dock small: HDR content comes out as SDR,
protected video is black, and the mouse pointer is never in the picture.

| PDFs | |
|---|---|
| Wheel · drag on the background or a scanned page · middle-drag | Scroll |
| Drag on text · double-/triple-click · Shift+click | Select text · word · line · extend |
| Shift+← → (Ctrl: by word) · Shift+Home End | Extend the selection |
| Ctrl+A · Ctrl+C · right-click | Select all · copy · Copy / Select all / minimap menu |
| Ctrl+F · Enter / F3 · Shift+Enter / Shift+F3 · Esc | Find · next · previous · close (the match stays selected) |
| Ctrl+wheel, pinch, Ctrl+= / Ctrl+− | Zoom at the cursor |
| 0 or Ctrl+0 · 2 · 1 | Fit page (the whole page) · fit width · actual size; also on the right-click menu |
| Ctrl+↑ Ctrl+↓ · ← → (when nothing to pan) · ˄ ˅ in the minimap | Previous / next page |
| Ctrl+G · click the page number | Go to page |
| Click / drag in the minimap | Go to that page / scroll |
| Ctrl+M | Show / hide the minimap |
| PgUp PgDn · Space / Shift+Space · Home End · arrows | Scroll by a screen (by a page at fit page) / scroll |

A PDF opens at the zoom used last on any PDF: pick fit page once (0), and every PDF
after opens with the whole page in view. Fit width is the starting point until a zoom
is picked. **Settings › PDFs & images** can fix the zoom instead: fit page, fit width or
actual size. A PDF opened before goes back to the page and zoom it was left at. These are
kept for the last 500 files, in `%LOCALAPPDATA%\whir\views.txt`. Each is kept only for
that same file: another file later saved at the same path, such as a mail attachment's
temporary copy, opens afresh. Settings can switch this off or forget them all.

The minimap is the document in miniature beside the scrollbar: page thumbnails with a
slider over what's on screen, the page number and previous/next buttons above them.
It scrolls along with the document, hides itself in narrow windows, and its on/off
state is kept in `%LOCALAPPDATA%\whir\settings.ini`. Jumping to a part of a long
document not seen yet shows each page's thumbnail at once while its sharp tiles render.

Search ignores case and line breaks (a hyphenated word split across lines still matches),
and results stream in while PDFium reads the pages; short documents are read in the
background as soon as they open, so Ctrl+F is instant. Pages with matches are marked
in the minimap.

| CSVs | |
|---|---|
| Click, Shift+click, drag | Select cells; click row numbers to select rows; top-left corner selects all |
| Arrows (+Shift to extend, +Ctrl to jump) · PgUp PgDn · Home End · Ctrl+A | Move / select |
| Ctrl+C | Copy the selection as TSV (pastes straight into Excel) |
| Click a header | Sort ascending → descending → original order |
| Drag / double-click a header border | Resize / fit a column |
| Ctrl+F · Enter / F3 · Shift+Enter / Shift+F3 · Esc | Find in the whole file · next · previous · close |
| Alt+W (or the "ab" button) in a find field | Whole cell: the cell must equal the text; numbers match by value (`2.45` finds `2.4500`) |
| Type · Enter / Shift+Enter · Tab / Shift+Tab · Esc | Edit the cell (starts with what you type) · commit and move down/up · right/left (Enter returns to where the Tabs started) · cancel |
| F2 / double-click · Backspace | Edit the cell's value (arrows move the caret; F2 toggles) · start empty |
| Alt+Enter · Ctrl+Enter (while editing) | Line break in the cell · put the value in every selected cell |
| Ctrl+X · Ctrl+V · Delete | Cut · paste tab-separated text (e.g. from Excel; grows the table) · clear |
| Ctrl+D · Ctrl+R | Fill down · fill right |
| Ctrl+Z · Ctrl+Y / Ctrl+Shift+Z | Undo · redo |
| Ctrl+Shift+= (Ctrl+numpad +) · Ctrl+− | Insert rows above (columns when whole columns are selected) · delete them |
| Shift+Space · Ctrl+Space | Select rows · columns |
| Type in the "+" row · click the "+" header | Add a row · add a column |
| Right-click row numbers, headers, cells | Insert/delete rows and columns, rename a column, sort / original order, cut/copy/paste/clear, find a cell's value in all tabs |
| Ctrl+S · Ctrl+Shift+S | Save · Save As (`.tsv` saves tab-separated) |

Edits sit on top of the file until you save, so even huge CSVs stay instant to edit.
Saving streams the file once: untouched rows are copied byte for byte (a one-cell change
changes one line), and quoting, line endings and encoding are kept. Sorting is a view —
it doesn't reorder the saved file. New tables (Ctrl+Shift+N) save into `Documents\whir`
and appear in the notes drawer.

Find skims the raw file for the text and only splits the rows it turns up in into cells,
so it runs at roughly disk speed: an 870 MB file takes about 0.3 s once it's in the OS cache.

### Find in all tabs

Ctrl+Shift+F opens a results panel along the bottom of the window (like Notepad++'s) and
searches every open table at once — say, a price across a week of rack files. Matches are
grouped by file, each with its row, column and the rest of the row; click one (or use ↑ ↓
in the list, or F4 / Shift+F4 from anywhere) to switch to that tab with the cell selected.
Click a file to fold it; drag the panel's top edge to resize it; Esc closes it. Each tab's
own find shows the same search, so F3 steps through that file's matches. Right-click a
cell → **Find "…" in all tabs** searches for its value as a whole cell.

## Time tracking

Time is tracked against projects, each a customer's, from the capture dock. After its
camera and delay buttons it is a stopwatch: ▶ / ⏸, today's time on the project to the
second, and the customer over the project.

| Dock | |
|---|---|
| ▶ · ⏸ | Time the project · pause (it carries on from the same time when played again, and starts over each day) |
| The customer and project | Pick another project: every customer's, with today's time on each. Picked, it starts; if another is running, the dock asks **Switch** or **Keep it running** first (one runs at a time) |
| ⌄ | Today's times: each project with time today, and the day's total (click it for the timesheet) |

While a timer runs the dock stays out, so the time is always in sight (an option), and the
tray icon's tooltip says what's running. Leave the PC (no keyboard or mouse for ten
minutes, a locked screen, sleep, or whir not running at all) and the dock asks, once you're
back, whether that time counts: **Keep it**, or **Drop it** to stop the timer when you left
and start it again from when you came back. Every second timed is kept, however short.

### The timesheet

Settings (Ctrl+, or the gear in the toolbar) opens on the week at a glance, and that's
where time is put down too: a row per project, grouped by customer with its code, a column
per day, in hours rounded the way they go on the timecard (to 0.25 h per project per day
by default; 0.1 h or exact in **Customers & projects**). The running project's today is
marked and counts up, and a line above the table shows it to the second, with **Pause**.

| Timesheet | |
|---|---|
| ‹ › · This week | Previous / next week · back to this one |
| Click a day · type `2`, `1.5`, `1:30` or `90m` · Enter | That day's hours on the project (more is added after the day's last entry; less is taken off the latest; empty or `0` clears it) |
| Tab / Shift+Tab · Esc | Put them down and go to the next / previous day · leave it as it was |
| The last row: Customer… then Project… | Add a project to the week (or a new customer or project, with its code), then type its hours |
| Below the table | The chosen day's entries: change a start or an end (`9:05`, `1:20 pm`, `1320`), the note, or delete it |
| Copy | The week as a table, to paste into Excel or an email |
| Close week… | Close it out: writes `<Monday> timesheet.csv` (the hours per project per day, with each day's notes) and `<Monday> entries.csv` (every entry, to the second) to the Time folder |
| Reopen · Closed weeks | Open a closed week again · every closed week with its hours, its CSV a click away (it opens in whir as a table) |

On Monday the timesheet says when last week is still open, with a way straight to it. A
closed week can be read but not changed until it's reopened.
**Customers & projects** is where timers are set up: customers, their projects and codes,
renamed in place; a project with time in it is archived (no longer offered in the dock,
its time kept) rather than deleted. `whir.exe --settings timesheet` (or `timers`,
`pdfs`, `startup`) opens a page directly; the day's total in the dock's ⌄ does that.

The timesheet is kept in `Documents\whir\Time` (`WHIR_TIME_DIR` elsewhere): the data in a
hidden `.whir` folder, one small text file for the projects and what's running and one
per week (Monday to Sunday) for its entries. Entries are kept to the second and never
span midnight (they're cut there), so every day's total is the sum of its entries.

## Building

Requirements: Rust (MSVC toolchain), Visual Studio Build Tools with the Windows SDK.

```powershell
scripts\fetch-pdfium.ps1   # pinned, SHA-256 verified PDFium build -> vendor\pdfium
cargo test --workspace
scripts\dist.ps1           # release build -> dist\whir\{whir.exe, pdfium.dll, licenses}
```

`cargo run -p whir -- <files>` works for development. Debug builds find `pdfium.dll` in `vendor\`.

Diagnostics:
- `WHIR_TRACE=1` appends a startup timeline (ms since process creation) to
  `%LOCALAPPDATA%\whir\trace.log`.
- `whir.exe --warp` renders with Windows' software rasterizer instead of the GPU driver.
- `WHIR_NOTES_DIR=<folder>` keeps notes somewhere other than `Documents\whir` (testing).
- `WHIR_CAPTURES_DIR=<folder>` does the same for screenshots.

## Measured (release build, desktop with NVIDIA GPU, files in the OS cache)

| | window on screen | content on screen | private memory |
|---|---|---|---|
| second double-click (hand-off to the running window) | — | ~14 ms in the new process | — |
| bare launch (blank note) | ~90 ms | ~150 ms | ~61 MB |
| 6000×4000 JPEG | ~90 ms | ~170 ms | ~104 MB |
| 20 MB / 300k-row CSV | ~90 ms | ~150 ms | ~67 MB |
| 66-page PDF | ~90 ms | ~160 ms | ~113 MB |
| same CSV, `--warp` | — | ~72 ms | ~34 MB |
| the capture dock (`--dock`), idle | — | — | ~4.5 MB (a frozen screen's worth more while selecting) |

On this machine the critical path is the GPU driver (`D3D11CreateDevice` ≈ 135 ms),
which runs in parallel with everything else. If the device isn't ready ~40 ms after the
window exists, whir shows the window immediately with a placeholder, and content follows.
Much of the baseline memory is the GPU driver itself. On weak or remote machines, compare
`--warp`.

## How it's built

```
crates/
  whir/           exe: startup ordering, single instance, tabs + title bar, toolbar, shortcuts,
                  save/close prompts, the bottom drawer, Settings, the Find in all tabs panel, --register
  whir-capture/   the capture dock (`--dock`): its own process, plain Win32 + GDI, no renderer
                  (its text through DirectWrite, in software); the tray icon, the stopwatch,
                  the project picker and today's times, noticing time away;
                  screen grab, the selection overlay, PNG into the screenshots folder
  whir-platform/  Win32 window: custom frame (tabs in the caption, Win11 snap layouts), event loop,
                  input → egui, Open/Save dialogs, Recycle Bin, session end, where the window opens
  whir-gfx/       D3D11 device + flip-model swapchain + a purpose-built egui painter (shaders compiled at build time)
  whir-core/      Document trait (incl. saving), lock-free-for-others file access and atomic
                  replace, the notes library, the screenshots store, marks, toolbar widgets,
                  find bar, f64 scrolling, theme tokens, tracing, the timesheet and the todos
                  (shared with the dock), settings.ini, where each file was left (views.txt)
  whir-notes/     Markdown notes styled as you type; autosave on a background writer
  whir-image/     WIC decoding (built into Windows), EXIF orientation, DCT-scaled JPEG decode, mipmapped GPU textures,
                  marks (highlights multiply-blended, text in egui's own glyphs, baked the same way
                  into copies), PNG encoding, thumbnails
  whir-pdf/       PDFium on one dedicated thread; pages rendered as tiles at exact device resolution;
                  fit page / fit width, opening at the zoom used last or where the file was left
  whir-csv/       quote-aware sparse row index (1 offset per 64 rows), virtualized grid, background
                  sort and a find that parses only rows containing the query,
                  an edit overlay (row piece table + cell overrides) and a streaming save
```

Principles:
- **Critical path first.** Decoding starts before the window exists, and the GPU initializes in
  parallel. PDFium loads only when a PDF is opened.
- **Event-driven.** No frame is drawn unless something changed, so idle CPU is 0%.
- **Never lock your files.** Everything is opened with full sharing and never memory-mapped, and
  handles are dropped when whir loses focus. Excel can save a CSV that's open in whir, and
  the CSV tab notices the change and offers to reload.
- **Never lose your work.** Saves go to a temp file next to the original and replace it
  atomically (`ReplaceFileW`, keeping permissions); notes save themselves; whir asks before
  closing anything unsaved, and waits for in-flight saves before exiting.
- **Memory follows the screen.**
  - Images keep a screen-sized level and decode full resolution only when you zoom in.
  - The CSV index costs about 1.25 MB per 10M rows.
  - PDF tiles live in an LRU with a 96 MB budget.
- **Two processes, one timesheet.** The dock and the window both change the timesheet and
  the todos: each change is made under a named mutex from a fresh read of the files, written
  atomically, and announced to the other process with a registered window message.

### Theming / re_ui

egui is pinned to `=0.36.2`, the version rerun's `re_ui 0.37` uses. All colours, sizes and
the font list are in `crates/whir-core/src/theme.rs`, which is the single seam for vendoring
re_ui's design tokens later. Fonts are loaded from `%WINDIR%\Fonts` (memory-mapped).
CJK and emoji fallbacks load on first use.

## Not yet

PDF passwords, animated GIFs, ICC colour management, image editing beyond highlights and
text, freehand marks, arrows and boxes, text fonts and sizes from a menu, tab reordering,
spreadsheet formulas, cell formatting and multiple sheets,
WebP/HEIC without the Microsoft Store codecs, IME composition at the caret, spell check,
subfolders in the notes library.
