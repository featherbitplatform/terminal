# Compact scrollback history: design

Date: 2026-10-24, revised 2026-10-25 with the owner's answers. Status: awaiting the owner's
approval.

Scope: the emulation library fork `feathercritty` (`../feathercritty`, our Apache-2.0 fork of
`alacritty_terminal` 0.26.0) and the parts of featherbit terminal (`fbterm`) that read its
history. Parent design: `2026-10-23-featherbit-terminal-design.md` (sections 5.1, 5.2, 5.12,
5.14, 6 and 7).

Items marked *(owner's decision)* were decided by the owner. Items marked *(builder's call)*
were decided while writing this design; the owner can override any of them, and the plan may
revisit one if a measurement shows a better option.

## 1. Problem

Every row of the grid, history included, is a `Row<Cell>` allocated at the full grid width. A
`Cell` is 24 bytes (`char` 4, `fg` 4, `bg` 4, flags 2, `Option<Arc<CellExtra>>` 8, padding 2),
so a history row costs `columns × 24` bytes plus a 32-byte header whatever it holds. Measured on
the owner's Apple M5 Max (scenario E in `dev/perf/README.md`, plus a per-tab breakdown measured
on 2026-10-24; phase 0 re-measures both and records them in `dev/perf/README.md`):

- 10,000 history lines at 126 columns: about 24 MB resident per tab (31-33 MB allocated). Line
  content and attributes barely change the cost; the window width sets it.
- 20 tabs with full scrollback (scenario E): 528 MB RSS, against the ~300 MB target of spec
  section 7. The process base is about 107 MB, so about 420 MB of that is history.
- A typical history row holds a short shell line or a 60-110 character log line, so most of
  those 24 bytes per cell repeat the same blank cell.

The owner accepted the 528 MB worst case for milestone 1 and listed compact scrollback as a
follow-up (spec section 6). This document is that follow-up.

## 2. Decided by the owner

- **Approach A: keep the library, change how history is stored inside our fork.** No switch to
  another emulation library. No disk spill for now. Disk spill may come later for histories
  beyond 10,000 lines; it would reuse the compact row format of this design, encrypted with an
  in-memory key (section 5.10 keeps the format ready for that).
- **Our own implementation, single license.** feathercritty stays Apache-2.0 only. The idea is
  credited to Adam Mikołajczyk (Razuer), who proposed compact history storage in
  alacritty/alacritty#9049, in the feathercritty README and in the module docs. No code from
  that proposal is used (section 4).
- **Never contribute to upstream `alacritty_terminal`.** The work lives in the fork only.
- **10,000 lines is the target size** (2026-10-25). The gates of section 6 are set at the
  10,000-line default. The `scrollback_lines` setting stays customizable over its whole range
  (0 to 1,000,000): larger histories must be correct, they may be slower to resize (section
  5.11), and the user is warned about that in Settings.
- **Changing the scrollback setting applies to open tabs** (2026-10-25), and lowering it warns
  the user first, because open tabs lose their oldest lines (section 5.11).
- Already decided earlier (spec 5.1): fbterm pins feathercritty by git commit; changes are made
  in the fork, pushed, CI'd, then the `rev` is bumped in fbterm.

## 3. Goals, non-goals, anti-goals

**Goals**

- Cut history memory by an order of magnitude for typical content (shell sessions, logs,
  compiler output): about 24 MB down to 1-3 MB per full 10,000-line tab at 126 columns.
- No change in emulation behavior: for any input, the screen, the history, the cursor,
  selections, search results and reflow are cell-for-cell what unmodified 0.26.0 produces.
- No slower parsing, typing or drawing at the bottom of the scrollback. Reading history
  (scrolling back, search, export, selection, resize) stays within the limits of section 6.
- Steady scrolling with a full history allocates nothing.
- Memory freed by closing a tab goes back to the OS.
- Changes concentrated in a few files of the fork, so upstream merges stay cheap.

**Non-goals (for now)**

- Disk spill of old history (later, owner's decision above).
- Making very large histories (100,000 lines and more) smooth to resize: a faster rewrap and
  coalescing of window-drag resizes can come later on the same format (section 5.6, phase 4).
  This work only makes them correct and measures their cost.
- A smaller `Cell` or a compressed visible screen. The screen stays as it is.
- General-purpose compression (LZ-family, zstd) of history rows.
- Fast paths that read text straight from the encoded form for search or export (allowed
  later, section 8 phase 4, only if a gate needs it).
- Changing the 10,000-line default, or measuring Windows and Linux memory (CI still runs every
  test on all three platforms).

**Anti-goals** (failures even if memory drops)

- Any observable change in emulation behavior, including in rare states (wide characters at
  wrap boundaries, background-colored erase, scroll regions, reflow edge cases).
- New work on the per-character write path, slower typing echo, or slower frames while the view
  is at the bottom.
- New `unsafe` code in feathercritty. Upstream's existing `unsafe` (`Row::new`,
  `Storage::swap`) stays as it is; the only new `unsafe` in the whole change is one FFI call
  in fbterm (section 5.9).
- A panic, or a wrong row, when anything reads history.
- Memory peaks during resize that exceed today's (rows decoded into a dense copy of the whole
  history), or memory that stays resident after a resize.
- Expensive host work triggered by remote programs (a program clearing its history must not
  make fbterm call into the allocator; section 5.9).

## 4. Clean-room rule and credit

**Rule for whoever builds this (people and AI tools alike):** do not open, read, fetch, clone
or copy any code from `github.com/tui-lipan/rozi`, from the patches published there, or from
any other implementation by the author of alacritty/alacritty#9049. That includes engine
patches, tests, benchmarks and scripts. The public text of issue #9049 may be read; everything in
this design is written from it and from the upstream 0.26.0 source, and builders work from this
design and that source only. If a tool offers to fetch that repository, decline.
If anyone has already looked at that code, they say so in the plan before writing any of this
module. Our encoding is different from the one proposed in #9049 (section 5.2), our tests and
benchmarks are our own, and all of it is Apache-2.0.

**Credit** (wording for the builder; the README entry and the module docs carry it):

> The idea of keeping the visible screen dense while storing history rows in a compact form,
> with pooled buffers and an explicit history read API, comes from Adam Mikołajczyk (Razuer),
> who proposed it in alacritty/alacritty#9049. feathercritty's implementation is independent and
> was written from the idea only.

## 5. Design

### 5.1 Overview *(builder's call: compact history is the only behavior)*

Since this is our fork, compact history is simply how the grid works: there is no opt-in flag,
no feature gate and no dense-history mode to maintain. (A temporary switch for A/B measurement is
not needed either: the benchmarks compare against unmodified 0.26.0 directly, section 6.3.)

The grid splits into two parts:

- **The screen**: the `lines` visible rows, dense `Row<Cell>`s in the existing `Storage` ring,
  exactly as today. Every write the parser makes lands here. The per-character path
  (`Term::input`, `Grid::cursor_cell`) is unchanged.
- **The history**: a new `History<T>` holding every row above the screen in a compact encoding
  (5.2) inside pooled byte blocks (5.3). A row is encoded once, when it scrolls off the top of
  the screen, and decoded only when something reads it.

The alternate screen grid has a history limit of 0, so it never encodes anything; full-screen
programs pay nothing new.

`Grid::scrolled_lines()` (fork change 2) keeps its exact meaning: it is incremented where rows
rotate into history and decremented by `grow_lines` as today, so fbterm's `StablePoint`s and
export positions work unchanged.

### 5.2 Row encoding *(builder's call: text plus attribute runs)*

Two encodings were weighed.

| | Prefix plus repeated tail (the #9049 idea) | Text plus attribute runs (chosen) |
|---|---|---|
| Form | the cells up to the last one that differs from the final cell, kept as `Cell`s, plus the final cell once; rows whose prefix exceeds half the width stay dense | the characters as UTF-8, one per cell; runs of cells sharing colors, flags and extra; the final repeated cell once |
| 20-character shell line at 126 columns | about 0.5 KB (×6 smaller) | about 0.07 KB (×40) |
| 95-character log line with 6 color runs | stays dense under the half-width rule, at best 2.3 KB (×1.3) | about 0.2 KB (×14) |
| Every cell a different color (worst case) | dense, 3 KB | about 2.3 KB, never above dense |
| Reads | can lend `&Cell` for every cell, so most read APIs stay as they are | rows are decoded into a caller's buffer; no `&Cell` into history |
| Complexity | small codec, needs a dense fallback and a width heuristic | larger codec (UTF-8, runs, wide-character marks), no fallback, no heuristic |

(Estimates from the cell layout; the gates of section 6.5 measure the real numbers.)

The deciding fact is the measured workload: the 528 MB case is about 100-character lines at 126
columns, exactly where prefix-plus-tail saves 0-25%. Text plus attribute runs gets the
24 MB-per-tab history down to about 2-3 MB for that content, which is also what spec section 6
already anticipated ("compress older history (text plus attribute runs), about 24-33 MB down to
1.5-2.5 MB per full tab"). The price is that history reads decode into a buffer instead of
borrowing cells. History is read rarely and in bulk (scrolled-back frames, search, export,
selection, resize), in row-sized steps, so one decode per row is affordable; section 5.4 shapes
the read API around that, and section 6.4 bounds the cost.

**The format.** A row is a self-contained byte record. Integers are LEB128 varints (one byte
below 128), colors are 4 bytes, flags 2 bytes, everything little-endian with no padding and no
pointers:

```
header   W   width (cells in the row)
         P   cells stored explicitly (0..P); cells P..W all equal the tail cell
         R   number of runs
         M   number of structural marks
         E   number of extras this row references (see 5.8)
         bits  WRAPS (cell W-1 has WRAPLINE), CLEAR (every cell is_empty())
text     UTF-8 of `c` for cells 0..P, exactly P characters (spacer cells included, '\t' kept)
runs     R × { cells, fg, bg, flags with the structural bits cleared, extra index or none }
marks    M × { column delta, structural bits } for each cell in 0..P with any structural flag
tail     the tail cell in full: c, fg, bg, flags (structural bits included), extra index or none
```

- **Structural flags** are `WIDE_CHAR`, `WIDE_CHAR_SPACER`, `LEADING_WIDE_CHAR_SPACER` and
  `WRAPLINE`. They are kept out of the runs, so a line of wide characters (which alternate
  `WIDE_CHAR` and `WIDE_CHAR_SPACER` cell by cell) stays one run plus a short mark list instead
  of one run per cell. Nothing is derived from Unicode widths: every flag is stored, so decoding
  is exact even in states the parser leaves behind after partial overwrites.
- **Colors** pack as a tag byte plus three payload bytes (`Named` by its discriminant, `Indexed`
  by its index, `Spec` as r, g, b). Unpacking a named color goes through an exhaustive `match`,
  so a new variant in `vte` is a compile error, and a unit test round-trips every variant. No
  transmutes.
- **Tail and `P`.** The tail is cell `W-1`. `P` is one past the last cell before it that differs
  from it (full `Cell` equality), so trailing blanks, and background-colored erased tails, cost
  one tail record. The scan starts at the row's `occ`: upstream documents that all cells from
  `occ` on are equal. That invariant is not kept by `Row::grow` today (it pads with default
  cells and leaves `occ` alone), so the builder makes `grow` raise `occ` to the old length, audits
  the other `Row` mutators, and debug builds verify after every encode that cells `P..W` equal
  the tail. In release the encode is O(`occ`), so a short line costs a few dozen comparisons
  regardless of width.
- **Runs** break where `fg`, `bg`, non-structural flags or the extra (by `Arc::ptr_eq`) change.
  Run lengths are varints, so there is no length limit.
- **Header bits** answer the common questions without decoding: `Grid::wraps(line)` reads
  `WRAPS`, `Grid::is_clear(line)` reads `CLEAR`.
- The encoder is one pass over `0..P` with an ASCII fast path for the text. There is no dense
  fallback: the worst case (a different style on every cell) is about 18 bytes per cell plus
  the header, below the 24 of a dense cell.

**Decoding** writes `W` cells into a caller-owned `Row<Cell>` (resized only when the width
changed): walk text and runs in step, then the tail, then apply the marks; set the decoded row's
`occ` to `W` (conservative, so a reset of a row that returns to the screen clears every cell).
Cells with an extra clone its `Arc`. Expected cost: well under a microsecond for a 126-column
row (measured in phase 2, gated in 6.4).

### 5.3 The history store *(builder's call: pooled byte blocks, a typed extras queue)*

- **Blocks.** Encoded rows are appended to fixed-size byte blocks (`Vec<u8>` with a fixed
  capacity; start at 64 KiB and tune between 16 and 256 KiB with the benchmarks). A row never
  spans two blocks; a row larger than a quarter block gets its own exactly-sized block. Blocks
  sit in a `VecDeque` in order, each with a count of live rows.
- **Index.** A `VecDeque` of small entries, one per history row (block sequence number, offset,
  first extra), oldest first. Line `-1` is the newest entry. With 10,000 rows the index is about
  150 KB.
- **Extras queue.** The `Arc<CellExtra>` values a row references are pushed, in order, onto one
  `VecDeque<Arc<CellExtra>>` shared by the whole history; a row's runs refer to them by index
  relative to the row's first extra. This keeps reference-counted pointers out of the byte
  blocks (no `unsafe`) and needs no allocation per row.
- **Append** (a row scrolls off the screen): encode into the last block, or into a spare block
  when it is full. **Evict** (history over its limit): pop the oldest index entry and its extras;
  a block whose live count reaches zero is removed from the front and kept as a spare.
- **Pool.** At most two spare blocks are kept. With a full history, every evicted block becomes
  the next spare, so steady scrolling allocates nothing: no blocks, no index growth, no extras
  growth, and the screen row that scrolled off is reset and reused as the new bottom row, as
  upstream does. A cleared or trimmed history (`clear_history`, `Grid::reset`, a lower
  `update_history`) frees every block it no longer needs back to the allocator and keeps at
  most the usual two spares for the output that follows; dropping the grid (closing the tab)
  frees everything. Whether that memory then goes back to the OS is fbterm's decision (5.9).
- **Changing a history row.** Rows in blocks are never edited in place. `pop_newest` (used when
  the screen grows and pulls rows back) decodes the newest row and, when its bytes are at the end
  of the last block, gives the bytes back. A change to a history row (5.5 has the one case)
  decodes it, applies the change, re-encodes it at the end of the last block and repoints the
  index entry; the old bytes become dead and are reclaimed when their block's live count reaches
  zero.
- The store is generic over the cell type through a small trait (`HistoryCell`: encode a row,
  decode a row, the shared-extra type). `Cell` implements the real format; the grid's unit tests
  that use `Grid<usize>` and `Grid<char>` implement a trivial byte format in the test modules.
  Block size is a parameter so tests can use tiny blocks (for example 256 bytes) to exercise
  block boundaries, spares and oversized rows constantly.

### 5.4 Grid API *(builder's call: screen-only indexing, explicit history reads)*

**Indexing reaches the screen only, and the compiler finds every call site.** `Grid` stops
implementing `Index<Line>`, `IndexMut<Line>`, `Index<Point>` and `IndexMut<Point>`. In their
place:

- `Grid::screen()` / `Grid::screen_mut()` return the screen (the existing `Storage` ring, or a
  thin handle over it) with `Index`/`IndexMut` for `Line` and `Point`, lines `0..screen_lines`.
  `cursor_cell()` and every write the parser makes go through it, compiling to the same code as
  today. The range check stays a `debug_assert!` as upstream has it, unless the ingest benchmarks
  show an always-on check costs nothing measurable (under 1%), in which case keep it on.
- History-capable reads, for any line from `topmost_line()` to `bottommost_line()`:
  - `RowReader<T>`: owns one scratch `Row<T>`; `reader.row(&grid, line) -> &Row<T>` returns the
    screen row itself, or decodes a history row into the scratch and returns that. This is the
    workhorse: every existing per-row loop (`line_to_string`, fbterm's renderer, search, export,
    selection) keeps its body and only changes how it gets `&Row`.
  - `Grid::cell(point) -> T`: one cell by value, for occasional point lookups
    (`Term::expand_wide`, the vi cursor in `RenderableCursor`).
  - `Grid::wraps(line)`, `Grid::is_clear(line)`: header bits, no decoding.
- `GridIterator` (used by `display_iter`, the library's regex, bracket, semantic, inline and
  line searches, and `clear_viewport`) yields `Indexed<T>` by value instead of `Indexed<&T>`. It
  keeps a lazily created `RowReader`, decodes a history row once when it enters it (never per
  cell), and `cell()` borrows from the iterator. Iterating the screen never allocates.

Why the stricter API: with `Index` gone from `Grid`, every place that reads or writes rows,
ours and every one added by a future upstream merge, fails to compile until someone decides
whether it touches history. A runtime check would only catch the paths a test happens to
exercise, and a missed one would crash fbterm or show the wrong row. The cost is a mechanical
edit of about 40 call sites in the fork (most in `term/mod.rs`'s handler, which become
`self.grid.screen_mut()[...]`) and about 10 in fbterm.

**Other grid surface:**

- `Dimensions::total_lines()` is `screen_lines + history.len()`; `history_size()`,
  `topmost_line()` and the rest keep their meaning.
- Upstream's resize buffer and cache rows (`MAX_CACHE_SIZE`, `Storage::initialize` beyond the
  screen) no longer exist for history; the pool takes over their job of avoiding allocations.
  `Storage` keeps its code and serves only the screen.
- `PartialEq for Grid` compares content: screen rows, every history row decoded, columns, lines,
  display offset.
- `serde`: the serialized form stays exactly upstream's (`raw.inner` rows in storage order,
  `zero`, `visible_lines`, `len`, plus the other fields), through a private proxy struct.
  Serializing decodes the history; deserializing encodes it. The ref-test data files do not
  change.
- The ref-test helpers `initialize_all` and `truncate` keep their observable result
  (`initialize_all` adds blank default rows at the oldest end until the history is at its limit).
- `Term::grid()`, `grid_mut()`, `inactive_grid()` and `scrolled_lines()` are unchanged.

### 5.5 The write path

- **`scroll_up` with the region starting at line 0** (normal output): for each of the
  `positions` rows leaving the top, encode the screen row into the history, evict the oldest row
  if over the limit, rotate the screen ring, reset the recycled row as upstream does, and count
  `scrolled_lines`. Fixed lines at the bottom of the region are swapped back exactly as upstream
  does. With a history limit of 0 nothing is encoded. Scroll counts at or above the region height
  must reproduce upstream's result (rows reset with the template's background entering history),
  which the differential test covers.
- **`scroll_up` with fixed lines at the top, and `scroll_down`:** screen-only, unchanged.
- **`clear_viewport`** scrolls the used part of the screen into history through `scroll_up`.
- **`clear_history`, `Grid::reset`, `update_history`:** drop or trim history from the oldest
  end; a lower limit evicts immediately.
- **`shrink_lines`** (fewer screen lines): the rows leaving the top of the screen are encoded
  into history oldest first, after upstream's cursor-keeping scroll.
- **`grow_lines`** (more screen lines): the newest `from_history` rows are decoded back onto the
  new top rows of the screen with `pop_newest`; the remaining logic (scroll for lines not
  available, cursor and display-offset updates, the `scrolled_lines` correction of fork change 2)
  is unchanged.
- **The one write into history:** `Term::input`, when overwriting a wide character in column 0
  or 1, clears `LEADING_WIDE_CHAR_SPACER` on the last cell of the line above; when the cursor is
  on line 0, that line is the newest history row. It goes through a history-row update (5.3).
  The compile errors of 5.4 make the builder classify every write; any other history write found
  uses the same update path.
- `swap_alt`, damage, the cursor, tab stops and modes are untouched.

### 5.6 Resize and reflow *(builder's call: upstream's algorithm over a streaming row sink)*

`grow_columns` and `shrink_columns` are the most intricate upstream code. They keep their
algorithm text as close to line-for-line as possible; only the containers change:

- **Input**: instead of `take_all()` returning every row as a dense `Vec`, the rows come from an
  iterator in the same order the upstream loop consumes them (oldest first), decoding history
  rows one at a time and popping them from the old store, so the old blocks free up as the pass
  proceeds.
- **Output**: instead of the `reversed`/`new_raw` vectors, a `ReflowSink` that supports exactly
  the operations the upstream code uses (`push`, `last_mut`, `len`, `truncate` from the newest
  end, `resize_with` blank rows, the final ordering and the `max_scroll_limit + lines` trim). It
  keeps the newest rows dense in a window (about `2 × lines + 2`) and encodes each row into a new
  history store when it leaves the window. Truncation past the window decodes rows back
  (`pop_newest`), so correctness does not depend on the window size; tests run with a window of
  1 to prove it.
- At the end, the newest `lines` rows become the screen and the rest is the new history.
- **Peak memory** is the old compact store plus the new one plus the window, far below today's
  peak (all rows dense). Nothing dense of history size is ever allocated, so a resize leaves no
  large freed region behind.
- The alternate screen grid (no history, no reflow) is unaffected.

Cost: every history row is decoded and re-encoded once per column change. Rows that reflow
leaves untouched could instead be copied as bytes with only the width patched (for growing: a
row whose tail is the default cell and whose previous row does not wrap; for shrinking: a row
with no carried cells, not the cursor line, `P` within the new width and either a default-empty
tail or an old width within the new one). That fast path is phase 4, built only if the resize
gate (6.4) needs it, and only with the differential test proving it.

### 5.7 Who reads history, and how

| Reader | Today | After |
|---|---|---|
| fbterm renderer (`widget/draw.rs`, `build`) | `&grid[line]` for each visible row | `RowReader` kept with the draw state across frames; decodes only the rows above line 0 while scrolled back (at most `screen_lines` per frame); at the bottom nothing is decoded |
| Damage tracking (`Term::damage`) | screen lines only | unchanged; scrolling the view already marks full damage |
| fbterm search (`search.rs`, `StablePoint`) | `grid[Line(l)]` per row, `wraps` reads the last cell | `RowReader` per chunk for `line_text`; `wraps` becomes `Grid::wraps` (header bit). Stable lines, chunking, the 3 ms lock budget and cancellation are unchanged |
| fbterm export (`export.rs`) | `lines.row(&g[Line(..)])` | `lines.row(reader.row(g, Line(..)))` per chunk; `inactive_grid()` path unchanged |
| fbterm selection text (`selection.rs`) | `&grid[line]` | `RowReader` |
| Library selection (`Selection::to_range`, `selection_to_string`, `bounds_to_string`, `line_to_string`) | `self.grid[line]` | `RowReader` in `line_to_string`; `to_range` itself reads no cells for `Simple` and `Block` |
| Library searches and vi mode (`term/search.rs`, `vi_mode.rs`; fbterm uses `semantic_search_left/right` for double-click) | `GridIterator` over `&Cell`, `grid[point]` | `GridIterator` over owned cells (decoded once per row), `Grid::cell`, `Grid::is_clear`; public signatures returning `Point` unchanged |
| `RenderableContent` | `display_iter` and the vi cursor cell | `display_iter` yields owned cells; the cursor reads `Grid::cell` |
| Resize | dense rows | 5.6 |

fbterm search's 1000-row chunks hold the terminal lock about 0.5 ms today; decoding adds to
that but the 3 ms budget and line-boundary yield already bound it.

### 5.8 CellExtra *(builder's call: keep the Arcs, store them once per run)*

`CellExtra` (zero-width characters, underline color, hyperlink) stays an `Arc` exactly as the
parser created it. A run refers to one extra; consecutive cells sharing the same `Arc` (a
hyperlink, a colored underline) are one run and one queue entry. A cell with combining
characters has its own `Arc` (the parser makes one per such cell), so it is its own run.
Decoding clones the `Arc`, so a read costs one atomic increment per cell that has an extra.
Rows full of combining characters or per-cell underline colors save less, and never cost more
than dense. Folding zero-width characters into the text stream (dropping those `Arc`s) is
possible later and not part of this work.

Hyperlink IDs generated without an explicit ID come from a process-wide counter in each crate;
the differential test (6.1) compares hyperlinks by URI and by the pattern of equal IDs, not by
the generated strings.

### 5.9 Returning memory to the OS (fbterm) *(builder's call)*

Dropping a history frees its blocks to the allocator, which keeps much of it resident. fbterm
adds one function, for example `fbterm::memory::release_freed()`:

- macOS: `malloc_zone_pressure_relief(NULL, 0)` (declared through `extern "C"`, one `unsafe`
  call with a `SAFETY` comment).
- Linux with glibc: `libc::malloc_trim(0)`.
- Everything else (musl, Windows, other targets): nothing.

It runs on tokio's blocking pool, at most once every 5 seconds (later requests within that time
fold into one), and only after user actions that free history: closing a tab or window once the
tab's last `SharedTerminal` reference is dropped (the session task may hold one; release after
it ends), lowering the scrollback setting (5.11), and any user-initiated clear added in the
future. A
program that clears its own history (`CSI 3 J`, `RIS`) does not trigger it: remote programs never
cause host work (owner's standing rule), and the freed blocks are reused by the next output anyway.
feathercritty itself never calls into the allocator.

### 5.10 Ready for disk spill later

Not built now (owner's decision), but the format keeps the door open: a row record contains no
pointers and refers to its extras by position, so a block plus the extras it references can be
serialized as is (extras as their zero-width characters, underline color and hyperlink ID and
URI) and encrypted with an in-memory key. Nothing in this work may add pointers or
process-specific values to the byte records.

### 5.11 The scrollback setting in fbterm *(owner's decision; details builder's call)*

Today `scrollback_lines` (0 to 1,000,000, default 10,000) applies only to tabs opened after the
change. With this work it applies to every open terminal's normal screen through
`Term::set_options` (which calls `update_history`), the same way `kitty_keyboard` already does
in `apply_settings`; the alternate screen keeps its limit of 0. Setting the options must not
re-announce the tab title (reuse the event filtering of `Terminal::set_kitty_keyboard`).

- **Raising it** applies at once: open tabs simply keep more history from then on.
- **Lowering it in the Settings window asks first.** The field saves as the user types, so a
  value on the way to a larger one (typing `5` before `50000`) must never trim anything. When the
  new value is lower than the saved one and at least one open tab holds more history lines than
  the new value, the value is not saved; the field shows, in the design system's warning style:
  "Open tabs will lose their oldest lines beyond N. This can't be undone." with **Apply** and
  **Cancel**. Apply saves and trims; Cancel restores the saved value. If no open tab would lose
  a line, the value saves as usual with no prompt.
- **Lowering it by editing `settings.toml`** applies when the file is saved, like every other
  setting, and the window's settings notice banner (`App::settings_notice`) says how many tabs
  were trimmed, for example "Scrollback lowered to 2,000 lines: 3 open tabs lost their oldest
  lines."
- Trimming is followed by the memory release of 5.9.
- **Large values warn.** Above 100,000 lines the Settings field shows a quiet note: "Large
  history: resizing a window can pause briefly while history rewraps." The value is still
  accepted. The threshold is a constant, set from the measurements of 6.8 (if 100,000 lines
  rewrap in under about 50 ms, the builder raises it to where the pause first becomes
  noticeable). The settings file comment and the docs site's settings page say the same.
- Search, export and the find bar need no change: they already handle rows leaving the history
  while they run (stable lines).

### 5.12 Rebase surface and upstream merges *(builder's call)*

Files in feathercritty:

- New: `src/grid/history.rs` (store, pool, extras queue, `RowReader`), `src/grid/compact.rs`
  (the row codec for `Cell` and the `HistoryCell` trait), `tests/differential.rs`, a
  `benches/` directory (6.3, 6.4).
- Changed: `src/grid/mod.rs` (struct, scroll paths, indexing, iterator, serde proxy, equality),
  `src/grid/resize.rs` (containers only), `src/grid/row.rs` (`occ` invariant),
  `src/grid/tests.rs` and `src/grid/storage.rs` tests (tests of removed cache internals are
  replaced by equivalent tests of the new store, each listed in the README entry),
  `src/term/mod.rs` (call sites, the history write, `line_to_string`), `src/term/search.rs` and
  `src/vi_mode.rs` (owned iterator items), `tests/ref.rs` (failure printout uses `RowReader`),
  `Cargo.toml` (dev-dependencies, version), `README.md`.
- Every changed file gets the fork's notice line at the top; new files carry it too.

Merging upstream follows the fork README (merge, never rebase; `main` commits stay reachable).
For this change in particular:

- New upstream code that indexes the grid will not compile after a merge; each site is
  classified (screen or history) like the original migration.
- Upstream changes to `resize.rs` are ported onto the sink containers; keeping the algorithm text
  aligned with upstream is what makes that a small diff.
- After adopting a new upstream release, the differential test's oracle moves to that release
  (dev-dependency `alacritty_terminal = "=X.Y.Z"`), and all gates of section 6 run again before
  fbterm bumps its `rev`.

## 6. Acceptance gates

All gates are measured in release builds on the owner's Apple M5 Max (macOS) unless stated, with
medians of at least 5 runs and the spread reported. Each gate names its pass condition.

### 6.1 Differential test (correctness oracle)

`tests/differential.rs` in feathercritty drives identical byte streams into unmodified
`alacritty_terminal` 0.26.0 (crates.io dev-dependency, exact version) and into feathercritty, and
compares them after every step:

- **State compared:** every screen and history row cell by cell through a projection (character,
  zero-width characters, fg, bg, flags, underline color, hyperlink URI and ID pattern); cursor
  point and `input_needs_wrap`; saved cursor; display offset; history size; columns and lines;
  mode bits; `selection_to_string` for random `Simple`, `Block`, `Semantic` and `Lines`
  selections that reach into history; `bounds_to_string` on random ranges; library regex search
  (`search_next` left and right from random points, patterns sampled from the text); `bracket_search`,
  `semantic_search_left/right`, `line_search_left/right` and `expand_wide` at random points;
  a few vi motions.
- **Steps:** random printable text (ASCII, Latin, CJK wide characters, emoji, combining marks),
  newlines and carriage returns, SGR (16, 256 and true colors, every flag, underline styles and
  colors), OSC 8 hyperlinks, erase in line and display (all modes, with background colors), insert
  and delete lines and characters, scroll regions (DECSTBM) with scrolling inside them, `CSI n S`
  and `CSI n T` including counts at and above the region height, tabs, DECALN, alternate screen
  on and off, `RIS`, `CSI 3 J`, scrolling the view while output arrives, resizes (1 and 2
  columns included, wide characters at every wrap boundary), history limit changes through
  `set_options` (0, 1, small values, growth).
- **Inputs:** a weighted random generator, plus realistic streams (the 45 ref-test recordings,
  deterministic generated colored `ls`-style listings, git-graph-style logs, compiler
  diagnostics, wide-character text, a full-screen program running on the primary screen).
- **Configuration:** history limits from 0 to 10,000, plus a pass at 100,000; tiny blocks and a reflow window of 1 in a
  second pass so block boundaries, spares, oversized rows and decode-back paths run constantly.
- **Runs:** deterministic seeds; a failure prints the seed and a minimized step list. CI: 300
  seeds × 400 steps per platform (Linux, macOS, Windows), in debug (assertions on). Before each
  fork release: a one-hour soak in release and one in debug, with seeds recorded in the PR.
- **Pass:** zero differences.

### 6.2 Existing tests

- All 45 upstream ref tests pass with their data files unchanged.
- All fork unit tests pass (replaced cache-internal tests listed in the README entry); the fork's
  CI (`cargo +nightly fmt --check`, clippy with `-D warnings`, `cargo test`,
  `cargo test --no-default-features`) passes on Linux, macOS and Windows.
- fbterm: the full gate (`cargo fmt --all --check`, clippy, `cargo test --workspace`,
  `cargo deny --all-features check`, live SSH tests) passes, with no test removed or loosened.
- fbterm golden outputs, recorded with the pre-change `rev` in phase 0 for fixed input streams
  (with and without the alternate screen up, after resizes): export in all four formats, the
  find bar's matches and counts, find-in-all-tabs results, selection text. Pass: byte-identical.

### 6.3 Ingest throughput (engine level)

A benchmark in feathercritty feeds pre-generated buffers (at least 50 MB each) to
`Processor::advance` on a `Term` with a full 10,000-line history, against unmodified 0.26.0 in
the same binary, at 126×37 and 200×60:

| Workload | Pass (median vs 0.26.0) |
|---|---|
| Short lines (0-20 characters, newline each, a few colors) | at most 10% slower |
| Full-width plain lines | at most 5% slower |
| Uniform rows (background-colored erase of whole lines) | at most 5% slower |
| Dense cells (a different color on every cell, scrolling) | at most 5% slower |
| Scrolling inside a region with fixed top and bottom lines (primary screen) | at most 5% slower |
| Full-screen redraws on the alternate screen | within 1% (no history work at all) |
| Typing echo (single characters and a prompt line per Enter) | within 1% |

The short-line allowance is the encoder's fixed cost per row; it is bounded by `occ` and must not
show at application level (6.6).

Allocations: a counting allocator shows **zero** allocation calls made by the grid per scrolled
row once the history is full, for plain and colored content (allocations the parser makes for
new `CellExtra`s are excluded and reported separately).

### 6.4 History reads

Same harness, full 10,000-row history at 126 columns, against 0.26.0:

| Read | Pass |
|---|---|
| fbterm search, full tab (`crates/fbterm-term/examples/search_bench.rs`, today 5 ms) | at most 1.5× today; 20 tabs in parallel (today 23 ms) at most 1.5× |
| Library regex search across the whole history, no match | at most 1.5× 0.26.0 |
| fbterm export, 10,000 rows of 200 columns (today 3 ms plain, 6.5 ms with colors) | at most 1.5× today |
| `selection_to_string` over 1,000 history rows | at most 1.5× 0.26.0 |
| Resize with reflow, 126→100→126 and 200→80 columns | at most 2× 0.26.0, or 10 ms, whichever is larger; peak allocated bytes during the resize below 0.26.0's |
| Resize of lines only (no column change) | at most 1.2× 0.26.0 |
| fbterm draw-list build with the view entirely in history | at most 1.15× the same content at the bottom |

### 6.5 Memory (library)

Live bytes of a `Grid` (counting allocator) after 12,000 lines into a 10,000-line history at
126 columns:

| Content | 0.26.0 (measured) | Pass |
|---|---|---|
| Short lines (≤ 20 characters, 1-2 color runs) | about 32 MB | ≤ 1.5 MB (≤ 150 bytes per row) |
| Log lines of about 95 characters with about 6 color runs | about 33 MB | ≤ 3 MB (≤ 300 bytes per row) |
| Lines of wide characters (CJK) | about 33 MB | ≤ 0.5× 0.26.0 |
| A different color on every cell, full width | about 33 MB | ≤ 1.0× 0.26.0 |
| Peak during the resizes of 6.4 | today's peak | below today's peak |

### 6.6 fbterm memory and CPU matrix (`dev/perf`)

`dev/perf/sessions.sh` gains two scenarios: **F**, 20 tabs each printing 12,000 short lines (as
E, with lines of at most 20 characters), and **G**, scenario E followed by closing 19 tabs and
reading RSS 10 s later. Every scenario also records footprint (`footprint <pid>`) alongside RSS,
since RSS misses GPU memory. Three runs each, medians, the window visible as today.

| Scenario | Today | Pass |
|---|---|---|
| E: 20 tabs, ~100-character lines, full history | 528 MB RSS | ≤ 200 MB RSS (history about 80 MB or less over B) |
| F: 20 tabs, short lines, full history | not measured (about 528 MB expected) | history (F minus B) ≤ 50 MB |
| G: E, then 19 tabs closed | not measured | RSS ≤ D plus one full tab plus 15 MB |
| A: 19 idle, 1 streaming the log | 144.5 MB, 21.9% CPU | RSS within 5 MB; CPU within 1 point of a core |
| B, D: idle | 107.1 MB, 103.0 MB, 0.00% | RSS within 2 MB, CPU 0.00% |
| C: `yes` flood | 168.7 MB, 104.4% | CPU within 5% relative; RSS no higher |

The baseline is re-measured in phase 0 on the same build profile, since load varies; the pass
conditions apply to that baseline.

### 6.7 The scrollback setting (fbterm)

Tests in fbterm, with the usual temporary config directory and in-memory secrets:

- Raising and lowering `scrollback_lines` changes the limit of every open terminal's normal
  screen, never the alternate screen's, and does not change the tab title.
- In the Settings window, typing a lower value saves nothing and shows the warning while an
  open tab would lose lines; Apply trims and saves; Cancel restores the field; typing through a
  lower value on the way to a higher one never trims.
- A lowered value written to `settings.toml` by hand trims open tabs and sets the notice with
  the number of tabs trimmed.
- Values above the large-history threshold show the note and are accepted.
- An own-window screenshot (`FBTERM_SCREENSHOT_WINDOW=settings`) of the warning and of the note
  is attached to the PR.

### 6.8 Large histories (correctness, measured not gated)

- A test in feathercritty fills a 1,000,000-line history at 126 columns, then resizes
  (126→100→126 columns and a lines-only resize), runs a regex search, reads the oldest and
  newest rows and lowers the limit to 10,000, checking each result against the same operations
  on unmodified 0.26.0 for 100,000 lines (the 1,000,000-line run checks invariants and the
  rows it reads, since a dense oracle that size costs gigabytes). It runs in release, marked
  `#[ignore]`, in a separate CI step on Linux and macOS.
- Recorded in the PR and in `dev/perf/README.md`, not gated: live bytes and RSS at 100,000 and
  1,000,000 lines for short and log-style lines, resize time at those sizes, and the time to
  trim 1,000,000 lines to 10,000. These set the warning threshold of 5.11 and tell whether the
  later large-history work is worth doing.

### 6.9 Done means

All of 6.1 to 6.8 pass; the results are in the feathercritty PR and in `dev/perf/README.md`;
spec sections 6 and 7 record the new numbers; the README change entry and the module docs carry
the credit of section 4.

## 7. Risks

| Risk | Mitigation |
|---|---|
| Reflow behaves differently in a rare state (wrapped wide characters, the cursor line, colored tails) | Upstream's algorithm text is kept and only its containers change; the differential test fuzzes resizes heavily, with a reflow window of 1 |
| A history read slips through as a screen read | Indexing removed from `Grid`, so every site is a compile error until classified; debug assertions; the differential test runs in debug |
| The `occ` invariant is wrong somewhere, so the encoder drops cells | `grow` fixed and mutators audited; debug builds verify every encode; the differential test |
| Short-line floods get measurably slower | Encode bounded by `occ`, ASCII fast path, zero allocations; gate 6.3 at engine level and 6.6 at application level |
| Scrolled-back frames, search or export get slower | One decode per row, header bits for `wraps`/`is_clear`, readers reused; gates in 6.4; phase 4 fast paths only if needed |
| Freed memory stays resident | Blocks freed whole, histories cleared drop spares, pressure relief after user actions; scenario G |
| Upstream merges get harder | Changes concentrated in `grid/`; compile errors flag new sites; the oracle moves with each upstream release |
| Hyperlink ID counters differ between the two crates in one process | Compare hyperlinks by URI and ID pattern |
| Clean-room contamination | Section 4; the plan states that the builder has not seen the #9049 code; no web fetching of that repository |
| `CellExtra`-heavy content saves little | Bounded by dense; acceptable; zero-width folding possible later |

## 8. Delivery

Feathercritty work happens on a `feature/compact-history` branch merged into its `main` through a
PR with green CI; fbterm work on `feature/compact-history` off `develop` (or off the milestone-1
branch while that has not merged). During development fbterm points at the working copy with a
`[patch."https://github.com/featherbitplatform/feathercritty"]` path entry that is never
committed; each fbterm bump follows `CLAUDE.md`: push the fork, wait for its CI, set the full
commit SHA as `rev`, run the full gate.

1. **Phase 0, harnesses and baselines (no behavior change).** feathercritty: the differential
   test (passing trivially against itself at this point), the ingest and read benchmarks and the
   memory measurement, with a baseline report in the PR. fbterm: golden outputs (6.2), scenarios
   F and G and footprint in `dev/perf`, baselines re-measured. No `rev` bump needed.
2. **Phase 1, the API split, history still dense.** feathercritty: remove `Grid` indexing, add
   `screen()`, `RowReader`, `cell`, `wraps`, `is_clear`, owned `GridIterator` items, the
   history write path and the serde proxy, with `History` storing dense rows. All tests, ref
   tests and the differential test pass; benchmarks show no change. Version `0.26.0+fbterm.2`.
   fbterm: bump `rev`, migrate its call sites (draw, search, export, selection, tests); goldens
   pass. Keeping the encoding out of this phase means a regression here is an API mistake, not a
   codec one.
3. **Phase 2, the compact store.** feathercritty: codec, blocks, pool, extras queue, reflow sink,
   `occ` invariant; gates 6.1 to 6.5. README change entry 5 ("Compact history") with the credit,
   module docs credit, version `0.26.0+fbterm.3`.
4. **Phase 3, fbterm.** Bump `rev`; memory release (5.9); the scrollback setting (5.11) with its
   docs-site and settings-file comment updates; run the `dev/perf` matrix; gates 6.2, 6.6, 6.7
   and the measurements of 6.8; update `dev/perf/README.md`, spec sections 6 and 7, and `CLAUDE.md` (gotchas: grid
   indexing is screen-only, history is read through `RowReader`).
5. **Phase 4, only if a gate is missed or marginal.** The byte-copy reflow fast path (5.6), a
   text fast path for search and export, encoder tuning. Each lands with the differential test
   green.

Later, separately and only when the owner asks: smooth very large histories (the byte-copy
reflow of 5.6 plus coalescing of window-drag resizes, gated at 100,000 and 1,000,000 lines),
and disk spill of the oldest blocks (5.10). Both build on this format without replacing it.

## 9. Owner's answers and open questions

Answered 2026-10-25: 10,000 lines is the target, the setting stays customizable with a warning
for large values; lowering the setting trims open tabs after a warning (both in section 2 and
5.11).

Answered 2026-10-26: scenario E measured 123 MB, so the main design's section 7 replaces the
528 MB acceptance with a standing target of at most 200 MB for 20 tabs with full default
scrollback. The scenario G miss on RSS (closing tabs leaves macOS 27's allocator cache resident)
is accepted because it is not a leak (`dev/perf/README.md`, scenario G).

No questions are open.
