# whir

A fast, lean, native Windows app for the files that matter at work: **images, PDFs,
CSVs and notes**. Double-click a file and it's on screen; open another and it becomes a
tab in the same window. Start whir on its own and you get a blank note to type into —
it saves itself, no questions asked. A small capture dock takes screenshots, keeps them,
and opens each one ready to highlight and label.

- Rust + raw Win32 + Direct3D 11 + egui (no eframe/winit/wgpu, no web runtime)
- One portable folder: `whir.exe` (~5.5 MB, static CRT) + `pdfium.dll` (~7 MB, loaded only when a PDF is opened)
- No installer, no admin rights, no VC++ redistributable

## Using it

**[docs/guide.md](docs/guide.md)** is the user guide: setting it up on a work PC, every
key and mouse action, the capture dock and time tracking, where things are kept, and the
command line. `scripts\dist.ps1` copies it into the portable folder as `README.md`, so it
travels with the app (once registered, it opens in whir itself).

In short: copy `dist\whir\` anywhere you can write, run `whir.exe --register` once, then
**Open with → Choose another app → whir → Always** for each file type.

## Building

Requirements: Rust (MSVC toolchain), Visual Studio Build Tools with the Windows SDK.

```powershell
scripts\fetch-pdfium.ps1   # pinned, SHA-256 verified PDFium build -> vendor\pdfium
cargo test --workspace
scripts\dist.ps1           # release build -> dist\whir\{whir.exe, pdfium.dll, README.md, licenses}
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

Find in a CSV skims the raw file for the text and only splits the rows it turns up in
into cells: an 870 MB file takes about 0.3 s.

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

What's not there yet is listed at the end of the guide.
