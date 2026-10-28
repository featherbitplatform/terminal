# Tunnels Panel Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the Tunnels window with a per-window panel docked at the right edge (sharing the edge with the file panel, one at a time) that lists every tunnel and highlights the focused pane's profile.

**Architecture:** The list logic of `tunnels_window` moves to a new `tunnels_panel` module and gains a highlight; per-window panel state lives on `App` like `FilePanels`; the window's existing right-edge dock (`Docked` in `window.rs`) shows either the file panel or the tunnels panel; the Tunnels window, its `App::tunnels_win`, `ShotWindow::Tunnels` and its shortcut mode are removed.

**Tech Stack:** Rust, iced 0.14, existing fbterm modules.

**Spec:** `docs/superpowers/specs/2026-10-28-tunnels-panel-design.md` (tunnel behaviour: `2026-10-27-tunnels-design.md`).

## Global Constraints

- Panel title `Tunnels`; rows, states, addresses, switch, Retry, search (fuzzy by tunnel and profile name) and the empty message exactly as the Tunnels window has them today.
- Opening the tunnels panel closes that window's file panel, and opening the file panel closes the tunnels panel.
- Highlight: the focused pane's profile group first, accent on its header; other groups keep profile order; no collapsing; a hidden (filtered out) group highlights nothing.
- Width shared across windows, persisted as `tunnels_panel_width` in `state.toml` (like `file_panel_width`, same limits); open/closed not restored at launch.
- Search text per window, kept while closed for the app session, dropped when the window closes; the search field takes focus on open; Escape clears the search, else gives the keyboard back to the terminal; Cmd+W / Ctrl+Shift+W still close the tab.
- The `Tunnels` action from a window without a dock raises the most recently focused terminal window and opens the panel there.
- `FBTERM_OPEN_TUNNELS=1` opens the panel in the first window; `FBTERM_SCREENSHOT_WINDOW=tunnels` is removed.
- A remote program never opens, closes or changes the panel or a tunnel. Never name other terminal applications; no emoji or em-dashes in copy. Conventional Commits with no attribution lines.

## Review Focus

- Toggling tunnels and file panel back and forth in one window while another window has its own panel open: each window keeps its own state. Test in Task 2 (`panels_are_per_window_and_exclusive`).
- The highlighted profile is deleted or renamed in the profile editor while the panel is open: the list rebuilds without it, no stale highlight. Test in Task 1 (`highlight_of_a_missing_profile_is_ignored`).
- A tunnel switched on from the panel asks its host-key or password question over the window the panel is in (unchanged behaviour). Test in Task 2 (`switch_from_panel_asks_over_its_window`).
- The panel's search field has focus and the user types: keys never reach the terminal and app shortcuts still work (as with the file panel). Test in Task 3 (`typing_in_panel_search_stays_in_the_panel`).
- The window is narrow: the dock's width limits keep the terminal usable, as for the file panel. Test in Task 3 (`panel_width_is_clamped_like_the_file_panel`).

---

### Task 1: Panel list with highlight

**Files:**
- Create: `crates/fbterm/src/tunnels_panel/mod.rs` (moved list logic from `tunnels_window/mod.rs`: `Row`, `Group`, `Tone`, `state_label`, `address`, `any_defined`, `palette_rows`, `indicator_count`)
- Modify: `crates/fbterm/src/main.rs` (module), every `crate::tunnels_window::` user
- Test: `crates/fbterm/src/tunnels_panel/tests.rs` (move the existing list tests there)

**Interfaces:**
- Produces: `Group { profile_id: ProfileId, profile: String, rows: Vec<Row>, highlighted: bool }`; `pub fn groups(store: &ProfileStore, tunnels: &Tunnels, query: &str, highlight: Option<ProfileId>) -> Vec<Group>`.

- [ ] **Step 1: Failing tests:** `highlighted_group_comes_first_others_keep_order`, `no_highlight_keeps_profile_order`, `filtered_out_highlight_highlights_nothing`, `highlight_of_a_missing_profile_is_ignored`, `local_profile_highlights_nothing`; existing list tests move and keep passing.
- [ ] **Step 2: Run** `cargo test -p fbterm tunnels_panel`; FAIL.
- [ ] **Step 3: Implement** by moving the module (keep `view.rs` for Task 3) and adding the highlight reorder.
- [ ] **Step 4: Run** `cargo test --workspace`; PASS.
- [ ] **Step 5: Commit** `refactor(tunnels): list logic for a panel, with a highlighted profile`.

### Task 2: Panel state, actions and the window's removal

**Files:**
- Create: `crates/fbterm/src/tunnels_panel/state.rs`
- Modify: `crates/fbterm/src/app.rs`, `actions.rs`, `shortcuts.rs`, `palette.rs` (Tunnels row), `file_panel/mod.rs` (`toggle_file_panel` closes the tunnels panel), `crates/fbterm-config/src/state.rs` (`tunnels_panel_width`), `debug.rs` (FBTERM_OPEN_TUNNELS), remove `tunnels_window/mod.rs` leftovers and `ShotWindow::Tunnels`
- Test: `crates/fbterm/src/tunnels_tests.rs` (window tests become panel tests)

**Interfaces:**
- Consumes: Task 1 `groups`.
- Produces: `pub(crate) struct TunnelsPanels { open: HashSet<window::Id>, query: HashMap<window::Id, String>, input: HashMap<window::Id, iced::widget::Id>, width: f32 }` on `App` as `tunnels_panels`; `App::toggle_tunnels_panel(&mut self, wid: window::Id) -> Task<Message>`; `App::last_terminal_window(&self) -> Option<window::Id>` (most recently focused terminal window, tracked on `WindowFocused`); messages `TunnelsMsg::{Close(window::Id), Query(window::Id, String), Toggle(window::Id, TunnelKey), Retry(window::Id, TunnelKey), Resize(f32), ResizeEnd}`.

- [ ] **Step 1: Failing tests:** `toggle_opens_and_closes_in_that_window_only`, `panels_are_per_window_and_exclusive` (tunnels opens: file panel closes; file panel opens: tunnels closes), `query_is_kept_while_closed_and_dropped_with_the_window`, `tunnels_action_from_settings_opens_in_last_terminal_window` (and raises it), `switch_from_panel_asks_over_its_window`, `width_is_persisted_and_clamped`, `open_tunnels_env_opens_the_panel` (debug aid), `no_tunnels_window_exists` (opening the action never creates a window).
- [ ] **Step 2: Run**; FAIL.
- [ ] **Step 3: Implement.** The tab bar `TabBarMsg::Tunnels` and `Action::Tunnels` call `toggle_tunnels_panel` for the window; from editor, Settings, Color schemes, archive windows the action targets `last_terminal_window` and returns a `window::gain_focus` task for it. Remove the Tunnels window paths and its shortcut mode; its close/quit handling goes away.
- [ ] **Step 4: Run** `cargo test --workspace`, clippy; PASS.
- [ ] **Step 5: Commit** `feat(tunnels): a panel per window instead of the Tunnels window`.

### Task 3: Panel view in the dock

**Files:**
- Modify: `crates/fbterm/src/window.rs` (the right-edge dock shows `DockContent::{Files, Tunnels}`; the tunnels button's active style), `crates/fbterm/src/tunnels_panel/view.rs` (from `tunnels_window/view.rs`, as a panel: header `Tunnels` with close button, search, groups, the highlighted header in `t.accent`), `shortcuts.rs` (panel keyboard: Escape clears search else gives keyboard back)
- Test: view-model tests in `tunnels_panel/tests.rs`; app tests in `tunnels_tests.rs`

**Interfaces:**
- Consumes: Task 2 state and messages.

- [ ] **Step 1: Failing tests:** `typing_in_panel_search_stays_in_the_panel`, `escape_clears_search_then_returns_the_keyboard`, `panel_width_is_clamped_like_the_file_panel`, `tunnels_button_shows_active_while_open`, `search_gets_focus_on_open` (the open returns a focus task for the panel's input id).
- [ ] **Step 2: Run**; FAIL.
- [ ] **Step 3: Implement** the dock content switch and the panel view; reuse the file panel's resize handle and width limits.
- [ ] **Step 4: Run** tests and take screenshots (temp `FBTERM_CONFIG_DIR` with two SSH profiles that define tunnels (unreachable sample hosts, never connected), `FBTERM_SECRETS=memory`, `FBTERM_OPEN_TUNNELS=1`, `FBTERM_SCREENSHOT=<scratch>/tunnels-panel.png`, perl alarm timeout): check the dock, the header, the rows and that nothing overflows.
- [ ] **Step 5: Commit** `feat(tunnels): tunnels panel view`.

### Task 4: Docs

**Files:**
- Modify: `website/docs/tunnels.md` ("The Tunnels window" section becomes the panel; replace or drop its screenshot reference so the build stays link-safe), `website/docs/keyboard-shortcuts.md`, `CLAUDE.md` (debug aids `FBTERM_OPEN_TUNNELS`, removed `FBTERM_SCREENSHOT_WINDOW=tunnels`, Status), `website/docs/file-panel.md` (one sentence: opening it closes the tunnels panel)

- [ ] **Step 1:** Write the docs from the spec.
- [ ] **Step 2: Run** `cd website && npm ci && npm run build`; PASS.
- [ ] **Step 3: Commit** `docs: tunnels panel`.
