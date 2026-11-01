# whir

A fast, lean Windows app for the files that matter at work: **images, PDFs, CSVs and
notes**. Double-click a file and it's on screen; open another and it becomes a tab in
the same window. Start whir on its own and you get a blank note to type into — it saves
itself, no questions asked. A small capture dock takes screenshots, keeps them, and
opens each one ready to highlight and label; it also times your work against projects
and keeps the week's timesheet.

This folder is the whole program: `whir.exe`, `pdfium.dll` (loaded only when a PDF is
opened) and `licenses`. No installer, no admin rights.

## Setting it up

1. Put this folder anywhere you can write (e.g. `%LOCALAPPDATA%\Programs\whir`).
2. Run once: `whir.exe --register`
   This adds per-user "Open with" entries for images, PDFs, CSVs, Markdown and text
   files (HKCU only).
3. Right-click a `.png`, `.pdf` and `.csv` → **Open with → Choose another app → whir →
   Always**. (Windows doesn't let programs make themselves the default; this click is
   required once per type.)

**Moving the folder:** quit whir from its tray icon, move the folder, then run
`whir.exe --register` again from the new place so double-clicked files find it. If whir
starts with Windows, that entry follows by itself the next time the dock runs. Notes,
screenshots, the timesheet and settings are kept outside this folder (see **Where
things are kept**), so they come along untouched.

**Removing it:** `whir.exe --unregister` removes the entries and stops whir starting
with Windows. Then quit whir from its tray icon (the dock is `whir.exe` too, and keeps
running when the window is closed) and delete the folder.

whir isn't code-signed, so Windows SmartScreen may stop it the first time: **More info →
Run anyway**. If your PC blocks unsigned programs outright (AppLocker/WDAC), try running
`whir.exe` from the folder first. If it's blocked there, it will be blocked everywhere.

## Everywhere

The menu bar (File, Edit, View, Tools, Help) has everything whir can do, each with its
shortcut. In a narrow window it folds into one button. Beside it are the tools of the
open document, and the strip along the bottom says how the document stands.

- **Ctrl+N · the + button**: new note
- **Ctrl+Shift+N · right-click the + button**: new table (CSV)
- **Ctrl+O**: open files
- **Ctrl+S · Ctrl+Shift+S**: save · save as / export a copy
- **Ctrl+E**: the drawer along the bottom: take it to the keyboard · fold it away
- **Capture, in the strip along the bottom · Tools › Capture screenshot**: take a screenshot
- **Tools › Capture dock**: open the capture dock
- **Ctrl+Shift+P · Tools › Command palette**: find anything by a few letters: every command in the menus, including the open document's, your notes, and the pages of Settings. ↑ ↓ and Enter pick, Esc closes
- **Ctrl+, · Tools › Settings**: Settings: the timesheet, customers & projects, time tracking, appearance, PDFs & images, capture & dock, startup & window
- **View › Theme · Settings › Appearance**: System, Light or Dark; the capture dock follows
- **Ctrl+Shift+F · F4 / Shift+F4**: find in all tabs · next / previous result, in any file
- **Ctrl+W / Ctrl+F4 / middle-click a tab**: close the tab (closing the last one closes whir)
- **The arrow after the last tab**: every tab, when there are more than fit
- **Right-click a tab**: close others · close tabs to the right · close all (leaves a blank note) · copy path · show in Explorer
- **Ctrl+Tab / Ctrl+Shift+Tab, Ctrl+PgDn / Ctrl+PgUp**: next / previous tab
- **Ctrl+1 … Ctrl+8, Ctrl+9**: go to tab n / last tab
- **Ctrl+Shift+T**: reopen a closed tab
- **Drag files onto the window**: open them

A ● on a tab means unsaved changes; closing it (or whir) asks whether to save.

The window opens where it was left: the same size and place, maximized if it was. If
the mouse is on another monitor, it opens there instead, centred, at the same size.
**Settings › Startup & window** can keep it exactly where it was instead. It is never
left off-screen when monitors change.

## Notes

**Settings › Appearance** sets how big a note's text is: Small, Medium or Large. It changes
how notes are shown here, not the files.

- **Title field · ↓ / Enter**: name the note (it becomes the file name) · go to the text
- **Enter in a list / task / quote**: continue it (Enter on an empty item ends it)
- **Tab / Shift+Tab**: indent / outdent list items or selected lines
- **Ctrl+Enter · click a box**: toggle a `- [ ]` task
- **Ctrl+B · Ctrl+I · Ctrl+K**: bold · italic · link (also in the toolbar, with list, task, undo and redo)
- **Ctrl+click a link**: open it
- **Ctrl+Z / Ctrl+Y**: undo / redo
- **Ctrl+Shift+D · Mark as todo in the toolbar**: flag the note as a todo · mark it done

Notes are plain Markdown files in `Documents\whir`, saved about a second after you stop
typing (and whenever you switch away). Other `.md` / `.txt` files — this one included —
open in the same editor and are saved with Ctrl+S. Clear a note completely and its file
goes to the Recycle Bin; Ctrl+Z, while the tab is still open, brings the note back.

A note flagged as a todo comes first in the notes drawer (⚑), the most recently flagged
first, and stays in sight on the capture dock: while any todo is open the dock stays out
with a card listing them (click one to open it in whir, ✓ to mark it done). Done ones go
back among the rest with a ✓. Flag, mark done or unflag from the toolbar, Ctrl+Shift+D,
right-clicking a note in the drawer, or F in the drawer. The flags are kept beside the
notes, in `Documents\whir\.whir\todos.txt`, so the notes stay plain Markdown; a renamed
note keeps its flag, and a deleted one takes it along.

## The drawer

The drawer along the bottom of the window is available everywhere, with **Notes** and
**Images** tabs. Notes lists the notes and tables in the notes folder, open todos first,
then newest first, wrapped into columns. Images shows screenshots as fixed-size
thumbnails in a grid, their highlights and text drawn on them. A taller drawer reveals
more rows rather than enlarging the thumbnails. The selected drawer tab stays selected
when you switch documents and is remembered next time you open whir. Both drawer tabs
share one height, so switching between Notes and Images keeps the panel the same size.

Opening or switching to a PDF or CSV folds the drawer down to its header by default.
Both drawer tabs stay visible: click either one or use Ctrl+E to unfold it without
leaving the document. This automatic fold doesn't change your fold preference for
notes and images.

- **Notes · Images**: select that library and unfold the drawer
- **Capture**: take a screenshot
- **Click the chevron or unused header space · drag its top edge**: fold or unfold · make it taller or shorter (more rows of images or notes); height is shared and remembered, while manual folding is remembered for each library
- **Click · right-click**: open · show in Explorer / delete (to the Recycle Bin)
- **Wheel over the images**: scroll through the thumbnail rows
- **Ctrl+E, then ↑ ↓ ← → · Enter · Delete · Esc**: keyboard: move · open · delete · give the keyboard back

## Images

- **The toolbar**: previous and next in the folder, zoom, rotate, highlighter, text, undo, redo and copy
- **Wheel, Ctrl+wheel, pinch**: zoom at the cursor
- **Drag**: pan
- **Double-click**: toggle fit / 100%
- **0 · 1 · + · −**: fit · 100% · zoom in · zoom out
- **← → · Home End · PgUp PgDn**: previous / next image in the folder (Explorer's sort order) · first / last
- **R · Shift+R · right-click**: rotate right · left (on screen only, for as long as the tab is open; the file isn't changed)
- **H · the highlighter in the toolbar** (hold or right-click it for colours): highlighter on / off
- **T · the text tool in the toolbar** (hold or right-click it for colours): text on / off
- **Highlighter on: drag**: draw a highlight
- **Text on: click**: type there (Enter: new line · Esc, Ctrl+Enter or a click elsewhere: done)
- **Either on: click · drag a selected mark · drag its handles · Delete · Esc**: select a highlight or text · move it · resize it · remove it · let go (selection, then the tool)
- **Either on: double-click text**: change what it says
- **Ctrl+Z · Ctrl+Y / Ctrl+Shift+Z**: undo · redo marks
- **Ctrl+C · right-click → Copy image**: copy the image as it is on screen, marks and rotation included
- **Ctrl+Shift+S · right-click → Save a copy…**: save that as a new PNG (Ctrl+S offers the same once there are marks)

An image opens fitted: a large one shrinks to the window, a small one stays at 100%.
**Settings › PDFs & images** can open each one at actual size instead, or at the zoom
used last on any image (← → through a folder too). The right-click menu has Fit to
window (0) and Actual size (1).

Highlights tint what is under them the way a marker does: white paper takes the colour,
black text stays black. Text is drawn with a thin halo (white around dark colours, dark
around white) so it reads on a busy screenshot; resizing it from a corner scales it.
Neither is ever drawn into the file. On a screenshot (in the screenshots folder) they
are saved as you make them and can be moved, resized, changed or removed any time; on
any other image they last as long as the tab. What leaves whir with the marks in it is a
copy: the clipboard, or Save a copy.

## Screenshots

The capture dock is a slim grip on a screen edge that slides out when the pointer rests
on it. It stays on top of every other window, full-screen ones included (only the Start
menu and Windows' notifications go over it). It is a small process of its own and is the
part of whir that keeps running: it starts with whir's window (and with Windows, if you
turn that on), stays when the window is closed, and has an icon in the notification
area. Quitting whir is done from that icon.

- **Camera · Ctrl+PrtSc from anywhere**: capture: the screen freezes and dims
- **…then drag · click a window · click the desktop**: take that region · that window · that monitor
- **Esc · right-click**: cancel
- **Stopwatch**: delay: off → 3 s → 5 s (time to open a menu or a tooltip before the freeze)
- **Drag the dock**: move it to any edge of any monitor (remembered)
- **Right-click the dock**: capture · delay · open whir · open the screenshots folder · hide the dock
- **Click the tray icon · right-click it**: open whir · capture, show/hide the dock, start with Windows on/off, **Quit whir**
- **In these menus**: the arrow keys and Enter pick, Esc or a click elsewhere closes

Each capture is saved as a PNG in `Documents\whir\Screenshots`, immediately added to the
drawer's **Images** tab, and opens in whir with the highlighter in hand. You don't need
to switch documents to refresh the thumbnail grid. Their highlights and text are kept
beside them (in `Screenshots\.whir`), so the PNG is always the capture itself.

Starting with Windows is off until you turn it on, in the tray menu or **Settings ›
Startup & window** (it's a per-user entry under
`HKCU\Software\Microsoft\Windows\CurrentVersion\Run`). **Settings › Capture & dock** changes the shortcut (press
the new one, with Ctrl or Alt), the delay, and whether the dock shows; the ⋯ beside the
shortcut has the Print Screen ones and **Turn off**. If another app already has that
shortcut, the page says so.

Captures are taken with GDI, which keeps the dock small: HDR content comes out as SDR,
protected video is black, and the mouse pointer is never in the picture.

## PDFs

- **The toolbar**: previous and next page, the page number (click to go to a page), zoom and fits, find, minimap
- **Wheel · drag on the background or a scanned page · middle-drag**: scroll
- **Drag on text · double-/triple-click · Shift+click**: select text · word · line · extend
- **Shift+← → (Ctrl: by word) · Shift+Home End**: extend the selection
- **Ctrl+A · Ctrl+C · right-click**: select all · copy · Copy / Select all / minimap menu
- **Ctrl+F · Enter / F3 · Shift+Enter / Shift+F3 · Esc**: find · next · previous · close (the match stays selected)
- **Ctrl+wheel, pinch, Ctrl+= / Ctrl+−**: zoom at the cursor
- **0 or Ctrl+0 · 2 · 1**: fit page (the whole page) · fit width · actual size; also on the right-click menu
- **Ctrl+↑ Ctrl+↓ · ← → (when nothing to pan) · ˄ ˅ in the minimap**: previous / next page
- **Ctrl+G · click the page number**: go to page
- **Click / drag in the minimap**: go to that page / scroll
- **Ctrl+M**: show / hide the minimap
- **PgUp PgDn · Space / Shift+Space · Home End · arrows**: scroll by a screen (by a page at fit page) / scroll

A PDF opens at the zoom used last on any PDF: pick fit page once (0), and every PDF
after opens with the whole page in view. Fit width is the starting point until a zoom
is picked. **Settings › PDFs & images** can fix the zoom instead: fit page, fit width or
actual size. A PDF opened before goes back to the page and zoom it was left at. These
are kept for the last 500 files, each only for that same file: another file later saved
at the same path, such as a mail attachment's temporary copy, opens afresh. Settings can
switch this off or forget them all.

The minimap is the document in miniature beside the scrollbar: page thumbnails with a
slider over what's on screen, the page number and previous/next buttons above them. It
scrolls along with the document and hides itself in narrow windows. Jumping to a part
of a long document not seen yet shows each page's thumbnail at once while its sharp
tiles render.

Search ignores case and line breaks (a hyphenated word split across lines still
matches), and results stream in while the pages are read; short documents are read in
the background as soon as they open, so Ctrl+F is instant. Pages with matches are marked
in the minimap.

## Tables (CSV)

- **The toolbar**: find, undo, redo, add a row or a column; a sorted table says so there, and a click puts it back in file order
- **Click, Shift+click, drag**: select cells; click row numbers to select rows; the top-left corner selects all
- **Arrows (+Shift to extend, +Ctrl to jump) · PgUp PgDn · Home End · Ctrl+A**: move / select
- **Ctrl+C**: copy the selection as TSV (pastes straight into Excel)
- **Click a header**: sort ascending → descending → original order
- **Drag / double-click a header border**: resize / fit a column
- **Ctrl+F · Enter / F3 · Shift+Enter / Shift+F3 · Esc**: find in the whole file · next · previous · close
- **Alt+W (or the "ab" button) in a find field**: whole cell: the cell must equal the text; numbers match by value (`2.45` finds `2.4500`)
- **Type · Enter / Shift+Enter · Tab / Shift+Tab · Esc**: edit the cell (starts with what you type) · commit and move down / up · right / left (Enter returns to where the Tabs started) · cancel
- **F2 / double-click · Backspace**: edit the cell's value (arrows move the caret; F2 toggles) · start empty
- **Alt+Enter · Ctrl+Enter (while editing)**: line break in the cell · put the value in every selected cell
- **Ctrl+X · Ctrl+V · Delete**: cut · paste tab-separated text (e.g. from Excel; grows the table) · clear
- **Ctrl+D · Ctrl+R**: fill down · fill right
- **Ctrl+Z · Ctrl+Y / Ctrl+Shift+Z**: undo · redo
- **Ctrl+Shift+= (Ctrl+numpad +) · Ctrl+−**: insert rows above (columns when whole columns are selected) · delete them
- **Shift+Space · Ctrl+Space**: select rows · columns
- **Type in the "+" row · click the "+" header**: add a row · add a column
- **Right-click row numbers, headers, cells**: insert/delete rows and columns, rename a column, sort / original order, cut/copy/paste/clear, find a cell's value in all tabs
- **Ctrl+S · Ctrl+Shift+S**: save · save as (`.tsv` saves tab-separated)

Edits sit on top of the file until you save, so even huge CSVs stay instant to edit.
Saving streams the file once: untouched rows are copied byte for byte (a one-cell change
changes one line), and quoting, line endings and encoding are kept. Sorting is a view —
it doesn't reorder the saved file. New tables (Ctrl+Shift+N) save into `Documents\whir`
and appear in the notes drawer.

Find skims the raw file for the text and only splits the rows it turns up in into
cells, so it runs at roughly disk speed, even on files of hundreds of megabytes.

### Find in all tabs

Ctrl+Shift+F opens a results panel along the bottom of the window and searches every
open table at once — say, a price across a week of rack files. Matches are grouped by
file, each with its row, column and the rest of the row; click one (or use ↑ ↓ in the
list, or F4 / Shift+F4 from anywhere) to switch to that tab with the cell selected.
Click a file to fold it; drag the panel's top edge to resize it; Esc closes it. Each
tab's own find shows the same search, so F3 steps through that file's matches.
Right-click a cell → **Find "…" in all tabs** searches for its value as a whole cell.

## Time tracking

Time is tracked against projects, each a customer's, from the capture dock. After its
camera and delay buttons it is a stopwatch: ▶ / ⏸, today's time on the project to the
second, and the customer over the project.

- **▶ · ⏸**: time the project · pause (it carries on from the same time when played again, and starts over each day)
- **The customer and project**: pick another project: every customer's, with today's time on each. Picked, it starts; if another is running, the dock asks **Switch** or **Keep it running** first (one runs at a time)
- **⌄**: today's times: each project with time today, and the day's total (click it for the timesheet)

While a timer runs the dock stays out, so the time is always in sight (an option), and
the tray icon's tooltip says what's running. Leave the PC (no keyboard or mouse for ten
minutes, a locked screen, sleep, or whir not running at all) and the dock asks, once
you're back, whether that time counts: **Keep it**, or **Drop it** to stop the timer
when you left and start it again from when you came back. Every second timed is kept,
however short.

### The timesheet

Settings (Ctrl+, or Tools › Timesheet) opens on the week at a glance, and that's
where time is put down too: a row per project, with its customer and code, a column per
day, in hours rounded the way they go on the timecard (to 0.25 h per project per day by
default; 0.1 h or exact in **Time tracking**). The running project's today is marked and
counts up, and a line above the table shows it to the second, with **Pause**.

The table fits itself to the window: the days narrow first, then customer and project
share a column. In a very narrow window it scrolls sideways.

- **‹ › · This week**: previous / next week · back to this one
- **Click a day · type `2`, `1.5`, `1:30` or `90m` · Enter**: that day's hours on the project (more is added after the day's last entry; less is taken off the latest; empty or `0` clears it)
- **Tab / Shift+Tab · Esc**: put them down and go to the next / previous day · leave it as it was
- **Add a project, in the last row**: find a project by a few letters of its customer or name, or make a new one, then type its hours
- **Below the table**: the chosen day's entries: change a start or an end (`9:05`, `1:20 pm`, `1320`), the note, or delete it
- **Copy**: the week as a table, to paste into Excel or an email
- **Close week…**: close it out: writes `<Monday> timesheet.csv` (the hours per project per day, with each day's notes) and `<Monday> entries.csv` (every entry, to the second) to the Time folder
- **Reopen · Closed weeks**: open a closed week again · every closed week with its hours, its CSV a click away (it opens in whir as a table)

On Monday the timesheet says when last week is still open, with a way straight to it. A
closed week can be read but not changed until it's reopened.

**Customers & projects** is where timers are set up: a card for each customer, with its
projects and codes. Click a name or a code to change it where it stands; Enter keeps it
and Esc puts it back. The ⋯ at the end of a row has the rest: a project with time in it is
archived (no longer offered in the dock, its time kept) rather than deleted, and archived
projects fold away at the bottom of their customer's card.

**Time tracking** has the rounding, when the dock asks about time away, and whether it
stays out while a timer runs.

Entries are kept to the second and never span midnight (they're cut there), so every
day's total is the sum of its entries.

## Where things are kept

Nothing is kept in this folder, so it can be moved or replaced with a newer build
without losing anything.

- `Documents\whir`: notes and new tables; the todo flags in `.whir\todos.txt`
- `Documents\whir\Screenshots`: captures; their highlights and text in `.whir`
- `Documents\whir\Time`: the timesheet (the data in a hidden `.whir` folder, one small text file for the projects and what's running and one per week, Monday to Sunday) and the CSVs of closed weeks. Set `WHIR_TIME_DIR` to keep it elsewhere.
- `%LOCALAPPDATA%\whir\settings.ini`: whir's settings, the theme and the minimap's on/off among them
- `%LOCALAPPDATA%\whir\dock.ini`: the dock's place, delay and hotkey (change the last two in **Settings › Capture & dock**)
- `%LOCALAPPDATA%\whir\views.txt`: where each PDF was left (the last 500)
- `%LOCALAPPDATA%\whir\crash.log`: if whir ever crashes, what went wrong (worth attaching to a bug report)

## Command line

- `whir.exe <files>`: open them (in the running window, if there is one)
- `--register` · `--unregister`: add · remove the "Open with" entries (unregistering also stops whir starting with Windows)
- `--settings timesheet` (or `timers`, `tracking`, `appearance`, `pdfs`, `capture`, `startup`): open that page of Settings
- `--dock` · `--dock-hide` · `--dock-start` · `--dock-capture` · `--dock-quit`: show the dock (even when hidden) · leave only its tray icon · start it without showing it · capture at once · close it
- `--warp`: draw with Windows' software renderer instead of the graphics driver (try it if the window doesn't draw properly, or on a weak or remote machine)

## Not yet

PDF passwords, animated GIFs, ICC colour management, image editing beyond highlights and
text, freehand marks, arrows and boxes, text fonts and sizes from a menu, tab
reordering, spreadsheet formulas, cell formatting and multiple sheets, WebP/HEIC without
the Microsoft Store codecs, IME composition at the caret, spell check, subfolders in the
notes library.
