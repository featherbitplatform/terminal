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

## Packaging

- `tests/` (reference test data, about 46 MB) and the `[[test]] ref` target in `Cargo.toml` were removed. Nothing else in the manifest was changed.
- `.cargo-ok` (a registry marker) was not copied.
