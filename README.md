# whir

A fast, lean, native Windows app for the files that matter at work: **images, PDFs,
CSVs and notes**. Double-click a file and it's on screen; open another and it becomes a
tab in the same window. Start whir on its own and you get a blank note to type into —
it saves itself, no questions asked.

- Rust + raw Win32 + Direct3D 11 + egui (no eframe/winit/wgpu, no web runtime)
- One portable folder: `whir.exe` (~3.8 MB, static CRT) + `pdfium.dll` (~7 MB, loaded only when a PDF is opened)
- No installer, no admin rights, no VC++ redistributable

## Using it on a work PC

1. Build (below) or copy `dist\whir\` anywhere you can write (e.g. `%LOCALAPPDATA%\Programs\whir`).
2. Run once: `whir.exe --register`
   This adds per-user "Open with" entries for images, PDFs, CSVs, Markdown and text files (HKCU only).
3. Right-click a `.png`, `.pdf` and `.csv` → **Open with → Choose another app → whir → Always**.
   (Windows doesn't let programs make themselves the default; this click is required once per type.)

`whir.exe --unregister` removes the entries. Deleting the folder removes the program.

If your PC blocks unsigned programs (AppLocker/WDAC), try running `whir.exe` from the
folder first. If it's blocked there, it will be blocked everywhere.

## Keyboard & mouse

| Everywhere | |
|---|---|
| Ctrl+N · `+` | New note |
| Ctrl+Shift+N · right-click `+` | New table (CSV) |
| Ctrl+O | Open files |
| Ctrl+S · Ctrl+Shift+S | Save · Save as / export a copy |
| Ctrl+E · rest the pointer on the right edge | Notes drawer |
| Ctrl+W / Ctrl+F4 / middle-click tab | Close tab (closing the last one closes whir) |
| Ctrl+Tab / Ctrl+Shift+Tab, Ctrl+PgDn / Ctrl+PgUp | Next / previous tab |
| Ctrl+1 … Ctrl+8, Ctrl+9 | Go to tab n / last tab |
| Ctrl+Shift+T | Reopen closed tab |
| Drag files onto the window | Open them |

A ● on a tab means unsaved changes; closing it (or whir) asks whether to save.

| Notes | |
|---|---|
| Title field · ↓ / Enter | Name the note (it becomes the file name) · go to the text |
| Enter in a list / task / quote | Continue it (Enter on an empty item ends it) |
| Tab / Shift+Tab | Indent / outdent list items or selected lines |
| Ctrl+Enter · click a box | Toggle a `- [ ]` task |
| Ctrl+B · Ctrl+I · Ctrl+K | Bold · italic · link |
| Ctrl+click a link | Open it |
| Ctrl+Z / Ctrl+Y | Undo / redo |

Notes are plain Markdown files in `Documents\whir`, saved about a second after you stop
typing (and whenever you switch away). Other `.md` / `.txt` files open in the same editor
and are saved with Ctrl+S.

| Notes drawer | |
|---|---|
| Rest the pointer on the right edge, then on the strip | Peek, then open the list (newest first) |
| Click · right-click | Open · Show in Explorer / Delete (to the Recycle Bin) |
| Ctrl+E, then ↑ ↓ · Enter · Delete · Esc | Keyboard: move · open · delete · close |

| Images | |
|---|---|
| Wheel, Ctrl+wheel, pinch | Zoom at the cursor |
| Drag | Pan |
| Double-click | Toggle fit / 100% |
| 0 · 1 · + · − | Fit · 100% · zoom in · zoom out |
| ← → · Home End · PgUp PgDn | Previous/next image in the folder (Explorer's sort order) · first/last |

| PDFs | |
|---|---|
| Wheel · drag on the background or a scanned page · middle-drag | Scroll |
| Drag on text · double-/triple-click · Shift+click | Select text · word · line · extend |
| Shift+← → (Ctrl: by word) · Shift+Home End | Extend the selection |
| Ctrl+A · Ctrl+C · right-click | Select all · copy · Copy / Select all menu |
| Ctrl+F · Enter / F3 · Shift+Enter / Shift+F3 · Esc | Find · next · previous · close (the match stays selected) |
| Ctrl+wheel, pinch, Ctrl+= / Ctrl+− | Zoom at the cursor |
| Ctrl+0 or 0 | Fit width |
| PgUp PgDn · Space / Shift+Space · Home End · arrows | Page / scroll |

Search ignores case and line breaks (a hyphenated word split across lines still matches),
and results stream in while PDFium reads the pages; short documents are read in the
background as soon as they open, so Ctrl+F is instant.

| CSVs | |
|---|---|
| Click, Shift+click, drag | Select cells; click row numbers to select rows; top-left corner selects all |
| Arrows (+Shift to extend, +Ctrl to jump) · PgUp PgDn · Home End · Ctrl+A | Move / select |
| Ctrl+C | Copy the selection as TSV (pastes straight into Excel) |
| Click a header | Sort ascending → descending → original order |
| Drag / double-click a header border | Resize / fit a column |
| Ctrl+F · Enter / F3 · Shift+Enter / Shift+F3 · Esc | Find in the whole file · next · previous · close |

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

## Measured (release build, desktop with NVIDIA GPU, files in the OS cache)

| | window on screen | content on screen | private memory |
|---|---|---|---|
| second double-click (hand-off to the running window) | — | ~14 ms in the new process | — |
| empty window | ~90 ms | ~150 ms | ~61 MB |
| 6000×4000 JPEG | ~90 ms | ~170 ms | ~104 MB |
| 20 MB / 300k-row CSV | ~90 ms | ~150 ms | ~67 MB |
| 66-page PDF | ~90 ms | ~160 ms | ~113 MB |
| same CSV, `--warp` | — | ~72 ms | ~34 MB |

On this machine the critical path is the GPU driver (`D3D11CreateDevice` ≈ 135 ms),
which runs in parallel with everything else. If the device isn't ready ~40 ms after the
window exists, whir shows the window immediately with a placeholder, and content follows.
Much of the baseline memory is the GPU driver itself. On weak or remote machines, compare
`--warp`.

## How it's built

```
crates/
  whir/           exe: startup ordering, single instance, tabs + title bar, shortcuts, save/close
                  prompts, the notes drawer, --register
  whir-platform/  Win32 window: custom frame (tabs in the caption, Win11 snap layouts), event loop,
                  input → egui, Open/Save dialogs, Recycle Bin, session end
  whir-gfx/       D3D11 device + flip-model swapchain + a purpose-built egui painter (shaders compiled at build time)
  whir-core/      Document trait (incl. saving), lock-free-for-others file access and atomic
                  replace, the notes library, find bar, f64 scrolling, theme tokens, tracing
  whir-notes/     Markdown notes styled as you type; autosave on a background writer
  whir-image/     WIC decoding (built into Windows), EXIF orientation, DCT-scaled JPEG decode, mipmapped GPU textures
  whir-pdf/       PDFium on one dedicated thread; pages rendered as tiles at exact device resolution
  whir-csv/       quote-aware sparse row index (1 offset per 64 rows), virtualized grid, background find/sort
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

### Theming / re_ui

egui is pinned to `=0.36.2`, the version rerun's `re_ui 0.37` uses. All colours, sizes and
the font list are in `crates/whir-core/src/theme.rs`, which is the single seam for vendoring
re_ui's design tokens later. Fonts are loaded from `%WINDIR%\Fonts` (memory-mapped).
CJK and emoji fallbacks load on first use.

## Not yet

PDF passwords, animated GIFs, ICC colour management, image editing, tab reordering,
WebP/HEIC without the Microsoft Store codecs, IME composition at the caret, spell check,
subfolders in the notes library.
