# Compact Scrollback History Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Store scrollback history rows in a compact byte encoding inside feathercritty, cutting a
full 10,000-line tab from about 24 MB to 1-3 MB with no change in emulation behavior, and make
fbterm's scrollback setting apply to open tabs.

**Architecture:** The grid splits into a dense screen (the existing `Storage` ring) and a
`History<T>` of encoded rows in pooled 64 KiB blocks. Indexing `Grid` is removed so the
compiler finds every history read; reads go through `RowReader`, `Grid::cell`, `Grid::wraps`,
`Grid::is_clear` and an owned-item `GridIterator`. Resize keeps upstream's reflow algorithm over
a streaming source and sink. fbterm migrates its call sites, releases freed memory after user
actions, and applies the scrollback setting live.

**Tech Stack:** Rust 2024; feathercritty (fork of `alacritty_terminal` 0.26.0, git dependency
pinned by `rev`); `alacritty_terminal = "=0.26.0"` from crates.io as a dev-dependency oracle;
iced 0.14 (fbterm); `libc` for `malloc_trim`.

**Spec:** `docs/superpowers/specs/2026-10-24-compact-history-design.md` (terminal repo). Every
task implements the section it cites; read that section before starting the task.

## Global Constraints

- **Clean room (spec section 4).** Never open, fetch, clone or copy anything from
  `github.com/tui-lipan/rozi` or other code by the author of alacritty/alacritty#9049. Work from
  the spec and the upstream 0.26.0 source only. Disclosure: during the 2026-10-24 research step a
  research subagent downloaded that repository's README files, benchmark reports and experiment
  patches into the session scratch folder and summarized them in prose; the design author worked
  from that prose summary (and chose a different encoding); the downloaded files were deleted on
  2026-10-25 unused. No builder may read the scratch research notes (`alacritty-memory-proposals.md`).
- Never open issues, PRs or comments on alacritty/alacritty. Depending on the crates.io
  `alacritty_terminal` crate as a dev-dependency is fine.
- feathercritty stays Apache-2.0 only. Every file it changes or adds starts with
  `// Modified by the featherbit authors for feathercritty; see README.md for the list of changes.`
  (`#` in TOML).
- **Merge-ready documentation in feathercritty (owner, 2026-10-25).** Every piece of code added
  or changed in feathercritty carries enough comments for someone merging a new upstream release
  to resolve conflicts without this plan: module docs for new files (what the module owns, its
  invariants, how it relates to the upstream code it replaced); at each changed upstream
  function or hunk, a short `// feathercritty:` comment saying what upstream did there, what we
  do instead and why, and what to re-check if upstream changes that code (for example "upstream
  indexed `self.raw[line]`; history rows are now read through `RowReader`; if upstream adds a
  read here, classify it as screen or history"). In `resize.rs`, mark each container call that
  differs from upstream so the algorithm text can be diffed against upstream. The README change
  entry lists every changed upstream file with a one-line note on what to watch when merging.
  Reviewers treat missing merge context on a changed upstream hunk as an Important finding.
- No new `unsafe` in feathercritty. The only new `unsafe` in the whole change is the
  `malloc_zone_pressure_relief` call in fbterm, with a `SAFETY` comment.
- feathercritty must not call into the allocator to release memory; a remote program (`CSI 3 J`,
  `RIS`) never triggers fbterm's release call.
- Byte records contain no pointers or process-specific values (spec 5.10).
- In the terminal repo: never name or compare to another terminal application (crate names in
  attribution are fine as already used); product copy has no em-dashes or emoji.
- fbterm test runs use a temporary `FBTERM_CONFIG_DIR` and `FBTERM_SECRETS=memory`; look at the
  app only through `FBTERM_SCREENSHOT` own-window screenshots.
- Gitflow and Conventional Commits in both repos. No attribution of any kind: no
  `Co-Authored-By` trailer and no session links in commits or PR bodies.
- Branches: feathercritty `feature/compact-history` off `main`, merged by PR with green CI.
  fbterm `feature/compact-history` off `feature/milestone-1` (it has not merged to `develop`).
  While developing, fbterm uses an uncommitted
  `[patch."https://github.com/featherbitplatform/feathercritty"] feathercritty = { path = "../feathercritty" }`
  entry in its root `Cargo.toml`; every committed `rev` bump points at a pushed commit whose
  fork CI is green.
- feathercritty checks: `cargo +nightly fmt --check`, `cargo clippy --all-targets -- -D warnings`
  (with and without `--no-default-features`), `cargo test`, `cargo test --no-default-features`.
  fbterm checks: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`, `cargo deny --all-features check`.
- Measurements: release builds on the owner's Apple M5 Max, median of at least 5 runs with the
  spread reported (3 runs for `dev/perf` scenarios).

## Review Focus

1. **Content that `CSI 3 J` / `RIS` clears while the find bar, an export or a selection is reading
   history.** Expected: readers see rows disappear exactly as today (stable lines go stale, no
   panic, no wrong row). Test added to Task 6 (`export_survives_history_clear_midway`) and Task 8
   (`history_clear_during_reader_use`).
2. **A window resized during heavy output with the view scrolled back.** Expected: same
   display offset clamping and cursor as 0.26.0. Covered by the differential generator's
   "scroll the view while output arrives" plus resize steps; Task 1 makes the generator
   interleave them explicitly (weight in step 3).
3. **Scrollback lowered while a tab is scrolled back past the new limit.** Expected: the view
   clamps to the oldest kept line, the find bar searches again, no panic. Test added to Task 13
   (`lowering_scrollback_clamps_scrolled_view`).
4. **Rows with hyperlinks and combining marks scrolled into history and then reflowed.**
   Expected: hyperlinks and zero-width characters survive, IDs keep their equality pattern.
   Test added to Task 7 (`roundtrip_extras_shared_and_unique`) and covered by the differential
   generator.
5. **Typing a scrollback value digit by digit in Settings.** Expected: no trimming until the
   user confirms; a transient lower value on the way to a higher one never trims. Test in Task 14
   (`typing_through_lower_value_never_trims`).

---

## File map

feathercritty (`../feathercritty`):

| File | Responsibility |
|---|---|
| `src/grid/history.rs` (new) | `History<T>`: blocks, index, extras queue, pool, append/evict/pop/update; `RowReader<T>` |
| `src/grid/compact.rs` (new) | `HistoryCell` trait; the `Cell` row codec; color packing |
| `src/grid/mod.rs` | `Grid` holds `screen: Storage<T>` + `history: History<T>`; screen-only indexing; new read API; owned `GridIterator`; serde proxy; `PartialEq` |
| `src/grid/resize.rs` | containers only: `RowSource`, `ReflowSink` |
| `src/grid/row.rs` | `occ` invariant in `grow` and audited mutators |
| `src/grid/storage.rs` | screen only; cache-internal tests replaced |
| `src/term/mod.rs`, `src/term/search.rs`, `src/vi_mode.rs`, `src/selection.rs`, `src/term/cell.rs` | call-site migration; the one history write |
| `tests/differential.rs` (new), `tests/differential/` (new module dir: `gen.rs`, `project.rs`, `minimize.rs`) | oracle test |
| `tests/large_history.rs` (new) | spec 6.8, `#[ignore]` |
| `benches/ingest.rs`, `benches/reads.rs` (new, `harness = false`), `benches/common/mod.rs` | 6.3, 6.4 |
| `examples/memory.rs` (new) | 6.5, counting allocator |
| `.github/workflows/ci.yml`, `README.md`, `Cargo.toml` | CI steps, change entry 5, version |

fbterm (`terminal/`):

| File | Responsibility |
|---|---|
| `crates/fbterm-term/tests/golden.rs` + `tests/golden/` (new) | golden outputs (6.2) |
| `crates/fbterm-term/src/{widget/draw.rs, search.rs, export.rs, selection.rs, widget/input.rs}` | migrated reads |
| `crates/fbterm-term/src/terminal.rs` | `Terminal::set_scrollback` |
| `crates/fbterm/src/memory.rs` (new) | `release_freed()` and its 5 s coalescing |
| `crates/fbterm/src/settings_window/{mod.rs, glue.rs, view code}` | live scrollback, confirmation, large-value note |
| `crates/fbterm-config/src/settings.rs` | `SCROLLBACK_WARN_ABOVE`, settings-file comment |
| `dev/perf/sessions.sh`, `dev/perf/README.md` | scenarios F, G, footprint |
| `website/docs/…settings page`, `CLAUDE.md`, spec sections 6 and 7 | docs |

---

## Phase 0: harnesses and baselines (no behavior change)

### Task 1: Differential test against 0.26.0

**Files:**
- Modify: `feathercritty/Cargo.toml` (dev-dependencies: `alacritty_terminal = "=0.26.0"`, `rand = "0.8"` with `rand_chacha` or `small_rng`)
- Create: `feathercritty/tests/differential.rs`, `tests/differential/{gen.rs,project.rs,minimize.rs}`

**Interfaces:**
- Produces: `Step` enum (`Bytes(Vec<u8>)`, `Resize{lines,columns}`, `ScrollView(i32)`, `SetHistory(usize)`, `Select{ty,start,end}` …), `gen::steps(seed: u64, n: usize, cfg: &GenCfg) -> Vec<Step>`, `project::Snapshot` (`PartialEq + Debug`) built from either crate's `Term`, `run(steps, limit) -> Result<(), Divergence{step, diff}>`, `minimize(steps) -> Vec<Step>`. Env knobs: `DIFF_SEEDS` (default 300), `DIFF_STEPS` (default 400), `DIFF_SEED` (replay one). Later tasks add `DIFF_TINY=1` (tiny blocks + reflow window 1, Task 8/9).

- [ ] **Step 1: Write the projection and runner.** `Snapshot` covers spec 6.1 "State compared" exactly: every screen and history row (character, zero-width chars, fg, bg, flags, underline color, hyperlink URI, hyperlink ID *pattern* = index of first cell with an equal ID), cursor point, `input_needs_wrap`, saved cursor, display offset, history size, columns, lines, mode bits, plus query results (`selection_to_string` for 4 random selection types reaching into history, `bounds_to_string`, `search_next` both directions with patterns sampled from the text, `bracket_search`, `semantic_search_left/right`, `line_search_left/right`, `expand_wide`, three vi motions from a random point). Query points come from the same seeded RNG for both terms.
- [ ] **Step 2: Write the generator** with the weights below; realistic streams from `tests/ref/*/alacritty.recording` (each replayed whole as one run), plus deterministic generated colored `ls` listings, git-graph logs, compiler diagnostics, CJK text and a full-screen program on the primary screen. Step kinds and weights: printable text 30 (ASCII/Latin/CJK/emoji/combining), newline/CR 15, SGR 10, OSC 8 5, EL/ED with bg 6, IL/DL/ICH/DCH 5, DECSTBM + scrolling inside 5, `CSI n S/T` (counts up to 2× region) 3, tab/DECALN 2, alt screen toggle 2, `RIS` 1, `CSI 3 J` 1, view scroll while output arrives 5, resize (1 and 2 columns included) 6, `set_options` history limit (0, 1, small, growth, 100,000 in a separate pass) 3. Never send more than 4,000 kitty keyboard pushes (fork change 1 is an intended difference).
- [ ] **Step 3: Write `minimize`** (drop halves, then single steps, while the divergence remains) and print `DIFF_SEED=<seed>` plus the minimized steps on failure.
- [ ] **Step 4: Run** `cargo test --release --test differential` with `DIFF_SEEDS=300 DIFF_STEPS=400`, then in debug. Expected: PASS (only fork change 1 differs, and the generator avoids it). A failure here is a harness bug: fix the harness, not the library.
- [ ] **Step 5: Add a `differential` step to `.github/workflows/ci.yml`** (debug, all three OSes; already covered by `cargo test`, so only set the env defaults) and commit: `test: differential test against alacritty_terminal 0.26.0`.

### Task 2: Benchmarks and memory measurement, baseline report

**Files:**
- Create: `feathercritty/benches/common/mod.rs`, `benches/ingest.rs`, `benches/reads.rs`, `examples/memory.rs`
- Modify: `feathercritty/Cargo.toml` (`[[bench]] harness = false` entries)

**Interfaces:**
- Produces: `common::workload(kind: Workload, bytes: usize, seed: u64) -> Vec<u8>` for `Workload::{ShortLines, FullWidthPlain, UniformRows, DenseCells, RegionScroll, AltScreenRedraw, TypingEcho, LogLines95, WideChars}`; `common::median_ms(runs, f)`; each bench prints a table `workload | 0.26.0 ms | fork ms | ratio | spread` and accepts `--gate` to exit non-zero on a missed threshold from spec 6.3/6.4; `examples/memory.rs` prints live bytes per content kind (spec 6.5 table) and peak bytes during the 6.4 resizes, using a `#[global_allocator]` counting wrapper over `System`, and an allocation-count mode for "zero allocations per scrolled row" (spec 6.3, last paragraph).

- [ ] **Step 1: Implement workloads and harness**, ≥50 MB buffers, sizes 126×37 and 200×60, history full (10,000) before timing, both crates fed in the same binary.
- [ ] **Step 2: Implement `benches/reads.rs`** for every row of spec 6.4 that is library-level (regex search no match, `selection_to_string` over 1,000 history rows, resize with reflow 126→100→126 and 200→80, lines-only resize). fbterm-level reads are Task 4.
- [ ] **Step 3: Run** `cargo bench --bench ingest`, `cargo bench --bench reads`, `cargo run --release --example memory`. Expected: ratios ≈1.0 (same code); memory rows match spec 6.5's "0.26.0 (measured)" column within 10%.
- [ ] **Step 4: Save the output** as `benches/BASELINE.md` (date, machine, commit) and commit: `test: ingest, read and memory benchmarks against 0.26.0`.

### Task 3: fbterm golden outputs

**Files:**
- Create: `terminal/crates/fbterm-term/tests/golden.rs`, `crates/fbterm-term/tests/golden/*.{txt,ansi,json}`

**Interfaces:**
- Produces: `golden::streams()` (fixed deterministic inputs: colored log, shell session, CJK + emoji, hyperlinks, a full-screen program left on the alternate screen) and checks per stream, at 126×37 and after resizes 126→80→126: export in `.txt`, `.ansi` (the `.gz` variants checked by decompressing), find-bar matches and counts for three fixed queries, find-in-all-tabs results across three terminals, selection text for four fixed selections. `UPDATE_GOLDEN=1` rewrites the files.

- [ ] **Step 1: Write the test** using fbterm-term's public `Terminal`, `export`, `search` and `selection` APIs (no app needed).
- [ ] **Step 2: Record** with `UPDATE_GOLDEN=1 cargo test -p fbterm-term --test golden` on the current `rev` (bc59f0620d23f7bd95c533c6d9d588f734f96e48).
- [ ] **Step 3: Run** without the variable. Expected: PASS. Commit: `test: golden outputs for export, search and selection`.

### Task 4: dev/perf scenarios F and G, footprint, re-measured baseline

**Files:**
- Modify: `terminal/dev/perf/sessions.sh`, `terminal/dev/perf/README.md`, `crates/fbterm-term/examples/search_bench.rs` (add an export timing and a scrolled-back draw-list build timing for spec 6.4's fbterm rows)

- [ ] **Step 1: Add scenario F** (20 tabs, 12,000 lines of at most 20 characters each, as E otherwise) and **G** (E, then close 19 tabs, read RSS 10 s later), and record `footprint <pid>` next to RSS in every scenario. Default `SCENARIOS` becomes `A B C D E F G`.
- [ ] **Step 2: Run** `bash dev/perf/sessions.sh` (about 35 min) and `cargo run --release -p fbterm-term --example search_bench`.
- [ ] **Step 3: Record** the baseline table (median and range, 3 runs) in `dev/perf/README.md` under a dated "Before compact history" heading. Commit: `perf: scenarios F and G, footprint, baseline before compact history`.

---

## Phase 1: the API split, history still dense

### Task 5: `History<T>` (dense) and the grid split

**Files:**
- Create: `feathercritty/src/grid/history.rs`
- Modify: `src/grid/mod.rs`, `src/grid/resize.rs`, `src/grid/storage.rs` (tests), `src/grid/tests.rs`, `src/term/mod.rs`, `src/term/search.rs`, `src/vi_mode.rs`, `src/selection.rs`, `src/term/cell.rs`, `tests/ref.rs`

**Interfaces:**
- Produces (stable from here on; Task 8 swaps internals only):
  - `pub struct History<T>`: `new(limit: usize) -> Self`, `len()`, `limit()`, `set_limit(&mut self, limit)` (evicts oldest), `push_newest(&mut self, row: &Row<T>)` (copies/encodes; the caller keeps its row for reuse), `pop_newest(&mut self, into: &mut Row<T>) -> bool`, `pop_oldest(&mut self, into: &mut Row<T>) -> bool`, `read(&self, i: usize, into: &mut Row<T>)` (i = 0 is the newest), `update(&mut self, i, f: impl FnOnce(&mut Row<T>))`, `wraps(i) -> bool`, `is_clear(i) -> bool`, `clear(&mut self)`.
  - `Grid::screen(&self) -> &Screen<T>` / `screen_mut(&mut self) -> &mut Screen<T>` where `Screen<T>` implements `Index/IndexMut` for `Line` (0..screen_lines, `debug_assert!`) and `Point`.
  - `pub struct RowReader<T>`: `new() -> Self`, `row<'a>(&'a mut self, grid: &'a Grid<T>, line: Line) -> &'a Row<T>`.
  - `Grid::cell(&self, point: Point) -> T` (T: Clone), `Grid::wraps(&self, line: Line) -> bool`, `Grid::is_clear(&self, line: Line) -> bool`.
  - `GridIterator<'a, T>` yields `Indexed<T>` by value; `cell(&self) -> &T` borrows from the iterator.
  - `HistoryCell` trait stub in `src/grid/history.rs` for now (`type Extra; fn is_wrapline(row) …` as needed by `wraps`/`is_clear`), moved to `compact.rs` in Task 7.

- [ ] **Step 1: Unit tests for `History<T>`** in `history.rs` (`Grid<usize>`-style rows): `push_evicts_oldest_at_limit`, `set_limit_lower_evicts_now`, `pop_newest_returns_last_pushed`, `read_zero_is_newest`, `update_changes_one_row_only`, `clear_empties`. Run, expect FAIL (no type), implement with `VecDeque<Row<T>>`, run, expect PASS.
- [ ] **Step 2: Replace `raw: Storage<T>` by `screen: Storage<T>` + `history: History<T>`** and remove `Index<Line>`, `IndexMut<Line>`, `Index<Point>`, `IndexMut<Point>` from `Grid`. Implement spec 5.4 and 5.5: `scroll_up` with region start 0 pushes each departing row into history (nothing at limit 0), counts `scrolled_lines`, then resets and reuses it as upstream; `clear_history`, `reset`, `update_history` per 5.5; `total_lines = screen_lines + history.len()`; `PartialEq` by content; serde through a private proxy struct with upstream's exact serialized fields (`raw.inner` in storage order, `zero`, `visible_lines`, `len`); `initialize_all` / `truncate` keep their observable result. `Storage` drops its history cache use.
- [ ] **Step 3: Resize in this phase:** collect history + screen into the existing dense `Vec` (oldest first), run the upstream algorithm unchanged, split back. (Task 9 replaces the containers.)
- [ ] **Step 4: Migrate every call site** the compiler reports. Screen writes become `self.grid.screen_mut()[…]`; history-capable reads use `RowReader`/`cell`/`wraps`/`is_clear`. The one history write (spec 5.5, last bullet: clearing `LEADING_WIDE_CHAR_SPACER` on line -1 in `Term::input`) uses `History::update`. List every site you classify as history-capable in the commit body.
- [ ] **Step 5: Replace storage cache-internal tests** with equivalent tests of the new layout; list each replaced test (old name → new name) in a scratch note for the README entry (Task 6).
- [ ] **Step 6: Run all fork checks plus** `cargo test --release --test differential` and `cargo bench --bench ingest` / `reads`. Expected: all PASS, 45 ref tests unchanged data, differential zero differences, bench ratios within noise (≤ 3%).
- [ ] **Step 7: Commit:** `refactor(grid): split screen and history behind an explicit read API`.

### Task 6: Release `0.26.0+fbterm.2` and migrate fbterm

**Files:**
- Modify: `feathercritty/{Cargo.toml, README.md}`; `terminal/Cargo.toml` (`rev`), `crates/fbterm-term/src/{widget/draw.rs, search.rs, export.rs, selection.rs, widget/input.rs, terminal.rs (tests)}`, `crates/fbterm-term/examples/spike.rs`, `crates/fbterm/src/tabs.rs`, `crates/fbterm-session/src/ssh/pool.rs` (test)

**Interfaces:**
- Consumes: Task 5's read API.

- [ ] **Step 1: Write the failing test** `export_survives_history_clear_midway` in `crates/fbterm-term/src/export.rs` tests: fill 5,000 lines, start an export with a 1,000-row chunk, feed `\x1b[3J` between chunks (test hook on the chunk loop), assert the export finishes, contains the rows read before the clear, and never a row twice or out of order.
- [ ] **Step 2: Fork side:** version `0.26.0+fbterm.2`; README change entry "5. Screen and history split (explicit history reads)" describing the API and listing replaced tests; push the branch, open the PR, wait for green CI, merge to `main`.
- [ ] **Step 3: fbterm side:** bump `rev` to the merged `main` commit. Migrate: draw.rs keeps one `RowReader` in the draw state across frames and decodes only lines < 0 (spec 5.7); search.rs `wraps` → `Grid::wraps`, `line_text` via a per-chunk `RowReader`; export.rs `lines.row(reader.row(g, Line(..)), …)`, and its `wraps` via `Grid::wraps`; selection.rs via `RowReader`; input.rs hyperlink lookups via `Grid::cell`; tests use `grid.screen()[…]`.
- [ ] **Step 4: Run** the fbterm checks and `cargo test -p fbterm-term --test golden`. Expected: PASS, goldens byte-identical, Step 1 test PASS. Commit: `refactor: read terminal history through feathercritty's read API`.

---

## Phase 2: the compact store (feathercritty)

### Task 7: Row codec and the `occ` invariant

**Files:**
- Create: `feathercritty/src/grid/compact.rs`
- Modify: `src/grid/row.rs`, `src/grid/history.rs` (trait moves out)

**Interfaces:**
- Produces: `pub(crate) trait HistoryCell: Sized + Clone + Default + PartialEq { type Extra; fn encode(row: &Row<Self>, out: &mut Vec<u8>, extras: &mut VecDeque<Self::Extra>) -> RowHeader; fn decode(bytes: &[u8], extras: &[Self::Extra] /* the row's slice */, into: &mut Row<Self>); }`, `pub(crate) struct RowHeader { width: u32, wraps: bool, clear: bool, extras: u32, len: u32 }`, `fn read_header(bytes: &[u8]) -> RowHeader`. `impl HistoryCell for Cell` with the exact format of spec 5.2 (field order: header W, P, R, M, E, bits; text; runs; marks; tail). Test-only impls for `usize` and `char` in the test modules.

- [ ] **Step 1: Failing tests** in `compact.rs`: `roundtrip_blank_row`, `roundtrip_short_ascii_line` (assert encoded len ≤ 80 bytes at 126 columns), `roundtrip_log_line_six_runs` (≤ 300 bytes), `roundtrip_every_cell_different` (≤ 24 × W), `roundtrip_wide_chars_and_spacers` (one run, marks for each structural cell), `roundtrip_leading_wide_spacer_and_wrapline`, `roundtrip_bg_colored_erased_tail` (P stops at the colored blank run), `roundtrip_tabs_kept`, `roundtrip_extras_shared_and_unique` (shared hyperlink `Arc` = one run, one queue entry; combining marks each own; decoded cells `Arc::ptr_eq` the originals), `color_pack_every_variant` (every `NamedColor`, `Indexed` 0..=255, `Spec`), `header_bits_without_decode`, and a seeded property test `roundtrip_random_rows` (10,000 rows, random cells including all flags) asserting `decode(encode(r)) == r` cell-for-cell with the decoded `occ == W`.
- [ ] **Step 2: `occ` invariant test** in `row.rs`: `grow_raises_occ_to_old_len`, plus one test per audited mutator (`shrink`, `reset`, `append`, `append_front`, `front_split_off`, `IndexMut` writes) asserting "all cells from `occ` on are equal". Run, expect `grow_raises_occ_to_old_len` FAIL.
- [ ] **Step 3: Implement** the codec (ASCII fast path for the text, LEB128 varints, exhaustive `match` for `NamedColor`, no transmutes) and the `grow` fix. Debug builds `debug_assert!` after each encode that cells `P..W` equal the tail.
- [ ] **Step 4: Run** `cargo test grid::compact grid::row`. Expected: PASS. Commit: `feat(grid): compact row codec`.

### Task 8: Pooled block store

**Files:**
- Modify: `feathercritty/src/grid/history.rs`, `src/grid/mod.rs` (constructor parameter), `tests/differential.rs` (`DIFF_TINY`)

**Interfaces:**
- Consumes: Task 7's `HistoryCell`.
- Produces: same `History<T>` API as Task 5 plus `History::with_block_size(limit, block_bytes)`; `pub(crate) const DEFAULT_BLOCK_BYTES: usize = 64 * 1024`; `MAX_SPARES = 2`; `Grid::new_with_block_size` (crate-private, tests and differential via a `#[doc(hidden)] pub fn` gated by `cfg(any(test, feature = "test-hooks"))`; add the `test-hooks` feature, off by default).

- [ ] **Step 1: Failing tests** with 256-byte blocks: `rows_never_span_blocks`, `oversized_row_gets_own_block` (> block/4), `evicted_block_becomes_spare_max_two`, `steady_scroll_allocates_nothing` (counting allocator in a test binary `tests/alloc.rs`: after the history is full, 10,000 more scrolled rows of plain and colored content make zero grid allocations), `pop_newest_gives_bytes_back_when_last`, `update_reencodes_at_end_and_reclaims_dead_bytes`, `clear_keeps_at_most_two_spares` (spec 5.3: cleared or trimmed history frees blocks it no longer needs and keeps at most two spares), `set_limit_lower_frees_blocks`, `index_entry_is_small` (`size_of` ≤ 16 bytes), `history_clear_during_reader_use` (a `RowReader` used across a `clear` returns no stale row: lines beyond the new `topmost_line` are not readable).
- [ ] **Step 2: Implement** blocks (`VecDeque<Block>` with sequence numbers and live counts), index (`VecDeque<Entry { block_seq, offset: u32, first_extra }>`), extras `VecDeque<T::Extra>`, append/evict/pop/update and the pool, exactly per spec 5.3.
- [ ] **Step 3: Run** unit tests, then `DIFF_TINY=1 cargo test --release --test differential` and the plain run. Expected: PASS, zero differences. Commit: `feat(grid): store history rows in pooled compact blocks`.

### Task 9: Streaming reflow

**Files:**
- Modify: `feathercritty/src/grid/resize.rs`, `src/grid/history.rs`

**Interfaces:**
- Produces: `struct RowSource<'a, T>` (iterator of `Row<T>` oldest first: history rows popped with `pop_oldest`, then screen rows), `struct ReflowSink<T>` with exactly `push(Row<T>)`, `last_mut() -> Option<&mut Row<T>>`, `len()`, `truncate_newest(n)`, `resize_with_blank(n, columns, template)`, `finish(self, lines, max_scroll_limit) -> (Storage<T>, History<T>)`; the dense window is `2 * lines + 2` rows by default, settable to 1 under `test-hooks`.

- [ ] **Step 1: Failing tests** in `src/grid/tests.rs`: `reflow_window_one_matches_default` (random grids, 1,000 resizes, window 1 vs default give equal grids), `reflow_peak_below_dense` (counting allocator: peak bytes during 126→100→126 with a full history are below the dense total of history rows), `reflow_frees_old_blocks_progressively`.
- [ ] **Step 2: Implement** by changing only the containers in `grow_columns` / `shrink_columns`; keep the upstream algorithm text line-for-line (diff against upstream `resize.rs` should show container calls only).
- [ ] **Step 3: Run** unit tests and both differential passes. Expected: PASS. Commit: `feat(grid): reflow history through a streaming source and sink`.

### Task 10: Large-history test (spec 6.8)

**Files:**
- Create: `feathercritty/tests/large_history.rs`

- [ ] **Step 1: Write** `#[ignore] large_history_matches_oracle_at_100k` (100,000 lines at 126 columns in both crates; resize 126→100→126 and lines-only; regex search; oldest and newest rows; lower to 10,000; compare `Snapshot`s from Task 1's projection), and `#[ignore] large_history_invariants_at_1m` (1,000,000 lines, fork only: history size, `topmost_line`, round-trip of 1,000 sampled rows against regenerated expected text, wraps/is_clear bits, resizes and trim keep size and ordering). Print timings and live bytes for spec 6.8's recorded table.
- [ ] **Step 2: Run** `cargo test --release --test large_history -- --ignored --nocapture`. Expected: PASS; copy the printed table into `benches/RESULTS.md`. Commit: `test: large history correctness and cost`.

### Task 11: Gates, credit and release `0.26.0+fbterm.3`

**Files:**
- Modify: `feathercritty/README.md`, `Cargo.toml`, `.github/workflows/ci.yml`, `src/grid/history.rs` and `compact.rs` (module docs), `benches/BASELINE.md` → `benches/RESULTS.md`

- [ ] **Step 1: Run the gates** of spec 6.3 (`cargo bench --bench ingest -- --gate`), 6.4 library rows (`reads -- --gate`), 6.5 (`--example memory -- --gate`), the allocation count. Tune block size between 16 and 256 KiB only if a gate needs it; record the choice.
- [ ] **Step 2: If any gate misses,** stop and do Task 16 (phase 4) for that gate before continuing.
- [ ] **Step 3: Credit:** module docs of `history.rs` and `compact.rs`, and README change entry "6. Compact history", carry the exact credit text of spec section 4. The README entry lists the format, the API, the replaced tests and the measured results.
- [ ] **Step 4: CI:** add a `DIFF_TINY=1` differential run and a release `cargo test --release --test large_history -- --ignored` step on Linux and macOS (Task 10's test).
- [ ] **Step 5: Soak:** one hour of differential seeds in release and one in debug (`DIFF_SEEDS` large, `DIFF_STEPS=400`); record seed ranges in the PR.
- [ ] **Step 6: Version `0.26.0+fbterm.3`,** commit `feat: compact scrollback history`, PR with `RESULTS.md` and the soak seeds, merge on green CI.

---

## Phase 3: fbterm

### Task 12: Bump to the compact store

**Files:**
- Modify: `terminal/Cargo.toml` (`rev`), `terminal/CLAUDE.md` (gotchas)

- [ ] **Step 1: Bump `rev`** to the merged `0.26.0+fbterm.3` commit; run fbterm checks, the live SSH tests (`FBTERM_TEST_SSH=1`, see `CLAUDE.md`) and goldens. Expected: PASS, goldens byte-identical.
- [ ] **Step 2: Run** `search_bench` (export, search, scrolled-back draw-list build) against spec 6.4's fbterm rows. Expected: within the thresholds.
- [ ] **Step 3: CLAUDE.md gotcha:** grid indexing is screen-only (`grid.screen()[…]`); history is read through `RowReader`, `Grid::cell`, `Grid::wraps`, `Grid::is_clear`. Commit: `build: feathercritty with compact history`.

### Task 13: Memory release and live scrollback setting

**Files:**
- Create: `terminal/crates/fbterm/src/memory.rs`
- Modify: `crates/fbterm/Cargo.toml` (`libc` for linux-gnu), `crates/fbterm-term/src/terminal.rs`, `crates/fbterm/src/settings_window/glue.rs` (`apply_settings`), tab/window close paths in `crates/fbterm/src/tabs.rs` / `windows.rs`, `crates/fbterm/src/sessions.rs`

**Interfaces:**
- Produces: `pub(crate) fn release_freed()` in `memory.rs` (schedules on the blocking pool, coalesces requests within 5 s into one; macOS `malloc_zone_pressure_relief(std::ptr::null_mut(), 0)` via `extern "C"`; `cfg(all(target_os = "linux", target_env = "gnu"))` `libc::malloc_trim(0)`; elsewhere a no-op); `Terminal::set_scrollback(&mut self, lines: usize) -> usize` (applies to the normal screen through `set_options`, drops the re-announced title event like `set_kitty_keyboard`, returns how many history lines were dropped); `Terminal::history_len(&self) -> usize`.

- [ ] **Step 1: Failing tests** in `terminal.rs`: `set_scrollback_raises_and_lowers_limit`, `set_scrollback_keeps_alt_screen_at_zero`, `set_scrollback_keeps_title`, `set_scrollback_reports_dropped_lines`, `lowering_scrollback_clamps_scrolled_view` (view scrolled to the top of 5,000 lines, lower to 1,000: display offset ≤ 1,000, no panic, `StablePoint`s for dropped lines resolve to `None`). In `memory.rs`: `release_requests_coalesce_within_five_seconds` (inject a clock and a counting backend). In app tests: `closing_tab_requests_release_after_last_reference`, `remote_history_clear_never_requests_release` (feed `\x1b[3J` and `\x1bc`, assert zero requests).
- [ ] **Step 2: Implement** `memory.rs`, `Terminal::set_scrollback`, and the triggers: tab/window close (after the session task drops its `SharedTerminal`), lowering the setting.
- [ ] **Step 3: `apply_settings`:** when `scrollback_lines` changes, call `set_scrollback` on every open tab's terminal (as `kitty_keyboard` does); if any lines were dropped, call `release_freed()`. For a change that came from the settings file (not the Settings window's confirmed Apply, Task 14), set `settings_notice` to `Scrollback lowered to {N} lines: {k} open tabs lost their oldest lines.` (`1 open tab` singular; N with thousands separators). Test `toml_lowering_trims_and_sets_notice`.
- [ ] **Step 4: Run** fbterm checks. Expected: PASS. Commit: `feat: apply the scrollback setting to open tabs and release freed memory`.

### Task 14: Settings window confirmation and large-value note

**Files:**
- Modify: `crates/fbterm/src/settings_window/mod.rs` (state and messages), its view code, `settings_window/tests.rs`, `settings_window/app_tests.rs`, `crates/fbterm-config/src/settings.rs` (`pub const SCROLLBACK_WARN_ABOVE: usize = 100_000;` and the settings-file comment), docs site settings page under `terminal/website/docs/`

**Interfaces:**
- Consumes: `Terminal::history_len`, `App::apply_settings` from Task 13.
- Produces: `SettingsMsg::ScrollbackApply`, `SettingsMsg::ScrollbackCancel`; state `pending_lower: Option<usize>`; the window needs `max_open_history: usize` (largest `history_len` over open tabs), refreshed by the app when the window opens and before a lower value is evaluated.

- [ ] **Step 1: Failing tests:** `lower_value_with_long_tab_asks_first` (no save, warning shown), `apply_saves_and_trims`, `cancel_restores_saved_value`, `lower_value_without_long_tab_saves_directly`, `typing_through_lower_value_never_trims` (type `5`, `50`, `500`, `5000`, `50000` from a saved 10,000 with a tab holding 8,000 lines: never trims, final value saves), `large_value_shows_note_and_saves` (150,000), `note_hidden_at_threshold` (100,000).
- [ ] **Step 2: Implement.** Copy, exactly: warning `Open tabs will lose their oldest lines beyond {N}. This can't be undone.` with buttons `Apply` and `Cancel` in the design system's warning style; note `Large history: resizing a window can pause briefly while history rewraps.` (quiet, secondary text). Enter on the field with a pending lower value does not apply it; it keeps the prompt.
- [ ] **Step 3: Threshold check:** use Task 10's 100,000-line resize time; if it is under about 50 ms, raise `SCROLLBACK_WARN_ABOVE` to the smallest measured size whose rewrap exceeds about 50 ms and record the measurement in the commit body.
- [ ] **Step 4: Docs:** settings-file comment for `scrollback_lines` and the docs site's settings page: applies to open tabs; lowering trims after a confirmation; large values may pause resizes.
- [ ] **Step 5: Screenshots** of the warning and the note via `FBTERM_OPEN_SETTINGS=1 FBTERM_SCREENSHOT_WINDOW=settings FBTERM_SCREENSHOT=<path>` (temporary config dir, memory secrets) for the PR.
- [ ] **Step 6: Run** fbterm checks and `cd website && npm run build`. Commit: `feat(settings): confirm before lowering scrollback, note large values`.

### Task 15: Measure and record

**Files:**
- Modify: `terminal/dev/perf/README.md`, `docs/superpowers/specs/2026-10-23-featherbit-terminal-design.md` (sections 6 and 7), `CLAUDE.md` (Status)

- [ ] **Step 1: Run** `bash dev/perf/sessions.sh` (A to G, 3 runs). Expected: spec 6.6 pass column (E ≤ 200 MB RSS; F history ≤ 50 MB over B; G ≤ D + one full tab + 15 MB; A, B, C, D within their tolerances of Task 4's baseline).
- [ ] **Step 2: Record** results, the 6.8 table and the large-history threshold in `dev/perf/README.md`; update spec sections 6 (compact scrollback done) and 7 (new numbers; the 528 MB acceptance stays until the owner answers the design's open question). Commit: `docs: compact history measurements`.
- [ ] **Step 3: Whole-branch review,** then report to the owner with the numbers and the open question of design section 9.

## Phase 4 (only if a gate in Task 11, 12 or 15 misses or is marginal)

### Task 16: Targeted fast paths

- [ ] For the missed gate only: the byte-copy reflow fast path of spec 5.6 (conditions exactly as listed there), a text fast path for search/export reading `text` from the record, or encoder tuning. Each lands with a new unit test for its conditions, both differential passes green and the gate re-run. Commit per fast path: `perf(grid): <fast path>`.
