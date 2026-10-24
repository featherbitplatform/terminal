# Changes to the vendored alacritty_terminal 0.26.0

This is the published `alacritty_terminal` 0.26.0 crate (Apache-2.0, license in `LICENSE-APACHE`), used through `[patch.crates-io]` in the workspace `Cargo.toml`. Drop the vendored copy and the patch entry once a release with an equivalent fix is adopted.

## 1. Keyboard mode stack eviction (crash fix)

`src/term/mod.rs`, `Term::push_keyboard_mode`. When the kitty keyboard protocol's flag stack is full (`KEYBOARD_MODE_STACK_MAX_DEPTH`, 4096), upstream evicts the oldest entry from the wrong stack:

```rust
// before
let removed = self.title_stack.remove(0);
// after
let removed = self.keyboard_mode_stack.remove(0);
```

With an empty title stack the original line panics (`Vec::remove(0)` on an empty vector), so any program or remote host that sends `CSI > flags u` 4097 times crashes the terminal's parser while the protocol is enabled.

## 2. Scroll counter on the grid (scrollback search)

`src/grid/mod.rs`: `Grid` gains a `scrolled_lines: i64` field (skipped by serde), incremented by `positions` where `Grid::scroll_up` rotates the whole buffer into history, and a getter `Grid::scrolled_lines()`. `src/grid/resize.rs`: `grow_lines` subtracts the lines it adds (lines pulled back from history move down the screen, and the rotation its `scroll_up` counts only undoes the growth), so the counter can go below zero (wrapping arithmetic). `scrolled_lines() + line` is a line number that stays the same while output scrolls the line upward and while the screen grows or shrinks, which the find bar uses to keep its matches and current match in place as new output arrives. Nothing upstream reads the field.

## 3. Inactive grid getter (tab export)

`src/term/mod.rs`: `Term::inactive_grid()` returns the grid that is not shown (the normal screen and its scrollback while the alternate screen is active). Exporting a tab writes the normal screen's history even while a full-screen program (vim, less, top) has the alternate screen up. Read-only; nothing upstream changes.

## Packaging

- `tests/` (reference test data, about 46 MB) and the `[[test]] ref` target in `Cargo.toml` were removed. Nothing else in the manifest was changed.
- `.cargo-ok` (a registry marker) was not copied.
