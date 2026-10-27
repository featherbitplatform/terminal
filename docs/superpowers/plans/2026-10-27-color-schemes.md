# Color Schemes and Pane Accents Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Named color schemes (built-in and user-made, shareable as files) applied globally or per profile, a graphical scheme editor with live preview, and profile accents on pane borders in split tabs.

**Architecture:** Scheme files and their store live in `fbterm-config` (`schemes.rs`, one TOML per scheme in `<config>/schemes/`, built-ins compiled in). The app keeps one `SchemeSet` and resolves each pane's scheme (profile override, else the global `color_scheme` setting, else Graphite) in `App::pane_style`, which already builds every pane's `TerminalStyle`; terminals get the scheme's colors through `ColorScheme::apply_to`. A new editor window (same pattern as the Settings window) edits user schemes with a live terminal preview. `Tiles` paints per-pane backgrounds, dim and accent borders.

**Tech Stack:** Rust 2024, iced 0.14 (no new features unless a task says so), `toml`/`toml_edit`, `rfd` (already used), existing fbterm crates.

**Spec:** `docs/superpowers/specs/2026-10-27-color-schemes-design.md` (refines `2026-10-23-featherbit-terminal-design.md` 5.10 and 6).

## Global Constraints

- Users who never touch schemes see exactly what they see today (default global scheme `graphite`, dark, in both app themes).
- A scheme never changes the app chrome (tab bar, dialogs, Settings, palette, editors keep the app theme); it rules only inside its sessions. The one place content meets chrome, the active tab's fade into the terminal, follows the active tab's focused pane background as it follows Graphite today.
- A missing or broken scheme never stops a session: fall back to the global scheme (Graphite for a missing global) and say so once in the settings notice.
- Remote programs cannot change which scheme applies or write scheme files.
- Scheme files: plain TOML, colors `"#rrggbb"`, no secrets, one file per scheme in `<config>/schemes/`; references by stable `id`.
- Built-in ids (fixed forever): `graphite`, `graphite-light`, `harbor`, `ember`. User scheme ids: `user-<uuid v4>`.
- Contrast bar for built-ins: foreground vs background >= 7.0; ANSI 1–6 and 9–14 vs background >= 4.5; selection vs foreground >= 4.5. Low contrast in user schemes is a non-blocking hint in the editor.
- Pane accent borders only when the tab shows more than one pane.
- Redraw contract (`CLAUDE.md`): a scheme change redraws affected visible panes once and, when the font changes, resizes them once through the existing per-pane resize path; nothing periodic.
- No new `unsafe`; cross-platform; no emoji or em-dashes in product copy; never name another terminal application; any palette taken from elsewhere needs an Apache-2.0-compatible license recorded in `LICENSES.md` (the built-ins here are original).
- Conventional Commits; no attribution of any kind (no `Co-Authored-By`, no `Claude-Session`, no claude.ai links).
- Gates per task: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`; `cd website && npm run build` when docs change.
- Copy (exact): Settings row `Color scheme`, button `Edit schemes…`; profile editor field `Color scheme`, first choice `Use global scheme`; window title `Color schemes · fbterm`; editor buttons `Duplicate`, `Rename`, `Delete`, `Import…`, `Export…`, `Save`; delete confirmation `Delete "{name}"?` with body `{n} profiles use it and will switch to the global scheme.` (`1 profile uses it and will switch to the global scheme.` singular; no body when unused) and buttons `Delete` / `Cancel`; contrast hint `Text may be hard to read on this background.`; palette action `Color schemes`.

## Review Focus

1. A profile refers to a scheme whose file was deleted outside the app: the pane opens with the global scheme and the settings notice names the profile and the missing id once. Test in Task 3 (`missing_profile_scheme_falls_back_with_notice`).
2. A scheme file with a bad color or missing keys dropped into `schemes/`: it is skipped with a notice naming the file and line; other schemes still load. Test in Task 1 (`bad_scheme_file_is_skipped_with_error`).
3. Two imported files with the same `id`: the second import asks replace-or-keep-both; keep-both gets a fresh id. Test in Task 6 (`import_same_id_keeps_both_with_new_id`).
4. Changing the font in a scheme used by a pane running a full-screen program: one resize reaches the session (debounced), never one per keystroke in the editor (edits apply to sessions on Save only). Test in Task 3 (`saving_a_scheme_font_resizes_its_panes_once`).
5. Editing the scheme a split tab's two panes use differently (one profile override, one global): only the affected pane changes. Test in Task 3 (`scheme_change_touches_only_its_panes`).

---

### Task 1: Scheme model, built-ins and store

**Files:**
- Create: `crates/fbterm-config/src/schemes.rs` (register in `lib.rs`), `crates/fbterm-config/tests/schemes.rs`
- Modify: `crates/fbterm-config/src/paths.rs`, `crates/fbterm-term/src/palette.rs`

**Interfaces:**
- Produces in `fbterm-config::schemes`:
  - `pub struct SchemeColors { pub foreground: Rgb, pub background: Rgb, pub cursor: Rgb, pub selection: Rgb, pub ansi: [Rgb; 16] }` (Clone, Copy, PartialEq, Eq, Debug).
  - `pub struct Scheme { pub id: String, pub name: String, pub colors: SchemeColors, pub font_family: Option<String>, pub font_size: Option<f32>, pub builtin: bool }` (Clone, PartialEq, Debug).
  - File shape (serde, TOML): top-level `id`, `name`, optional `font_family`, `font_size`, and a `[colors]` table with `foreground`, `background`, `cursor`, `selection`, `black`, `red`, `green`, `yellow`, `blue`, `magenta`, `cyan`, `white`, `bright_black` … `bright_white`. `builtin` is never written.
  - `pub const BUILTIN_IDS: [&str; 4] = ["graphite", "graphite-light", "harbor", "ember"]`; `pub fn builtins() -> Vec<Scheme>`; `graphite` equals today's `ColorScheme::graphite()` values exactly; `graphite-light` derives from the design system's light tokens; `harbor` (cool blue-green) and `ember` (warm red-orange, meant for "production") are original palettes, all four named as shown in title case (`Graphite`, `Graphite Light`, `Harbor`, `Ember`).
  - `pub struct SchemeSet { … }` with `pub fn load(dir: &Path) -> (SchemeSet, Vec<ConfigError>)` (built-ins first, then user files sorted by name; a bad file is skipped with a `ConfigError` carrying path and line; a user file with a built-in id or a duplicate id is skipped with an error), `get(&self, id: &str) -> Option<&Scheme>`, `all(&self) -> &[Scheme]`, `users_of`-free (the app counts profiles).
  - `pub fn save_user(dir: &Path, scheme: &Scheme) -> Result<PathBuf, ConfigError>` (refuses built-ins; file name `<id>.toml`; atomic write), `pub fn delete_user(dir: &Path, id: &str) -> Result<(), ConfigError>`, `pub fn read_file(path: &Path) -> Result<Scheme, ConfigError>`, `pub fn write_file(path: &Path, scheme: &Scheme) -> Result<(), ConfigError>`, `pub fn new_user_id() -> String`, `pub fn contrast_ok(colors: &SchemeColors) -> bool` (the Global Constraints bar).
  - `Paths::schemes(&self) -> PathBuf` = `config_dir/schemes`.
- Produces in `fbterm-term::palette`: `impl From<&SchemeColors> for ColorScheme`; `ColorScheme::graphite()` stays and a test pins it equal to the `graphite` built-in.

- [ ] **Step 1: Write failing tests** (`crates/fbterm-config/tests/schemes.rs` and unit tests): `builtins_have_fixed_ids_and_names`, `graphite_builtin_matches_today`, `builtins_meet_the_contrast_bar`, `user_scheme_round_trips` (save then load gives an equal `Scheme` with `builtin: false`), `bad_scheme_file_is_skipped_with_error` (one valid and one with `background = "red"`: one loads, one `ConfigError` with the file path and line), `builtin_id_in_user_file_is_skipped`, `duplicate_user_id_is_skipped`, `save_refuses_builtins`, `delete_removes_the_file`.
- [ ] **Step 2: Run** `cargo test -p fbterm-config schemes` — Expected: FAIL (module missing).
- [ ] **Step 3: Implement** the interfaces above.
- [ ] **Step 4: Run** the gates — Expected: PASS.
- [ ] **Step 5: Commit** `feat(schemes): scheme files, built-ins and store`.

### Task 2: Global setting and profile override

**Files:**
- Modify: `crates/fbterm-config/src/settings.rs` (field, template), `settings_edit.rs` (`SettingChange`), `profile.rs`, `shells.rs`, every `Profile { … }` literal, `website/static/examples/profiles.toml` only if the example should show the field (no).

**Interfaces:**
- Produces:
  - `Settings.color_scheme: String`, default `"graphite"`; template lines after `theme`: `# Terminal color scheme for every session (a scheme id; see the Color schemes window).` / `#color_scheme = "graphite"`.
  - `SettingChange::ColorScheme(String)` with key `color_scheme`.
  - `Profile.color_scheme: Option<String>` with `#[serde(default, skip_serializing_if = "Option::is_none")]`.
  - No validation of ids here (the config crate doesn't know the app's set); Task 3 reports unknown ids.

- [ ] **Step 1: Write failing tests**: `color_scheme_defaults_to_graphite`, `color_scheme_write_field_round_trips` (via `Settings::write_field`, comments kept), `profile_color_scheme_round_trips_and_is_omitted_when_none`, `old_profiles_file_loads_without_color_scheme`.
- [ ] **Step 2: Run** — Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the gates — Expected: PASS.
- [ ] **Step 5: Commit** `feat(schemes): global color_scheme setting and per-profile override`.

### Task 3: Apply schemes to panes

**Files:**
- Modify: `crates/fbterm/src/app.rs` (`App.schemes: SchemeSet`, load at build), `settings_window/glue.rs` (`terminal_style`, `pane_style`, `apply_settings`), `sessions.rs` (`new_terminal`, `start`), `tabs.rs` (`Pane.scheme: Option<String>` copied from the profile), `dialogs.rs` (`profiles_changed`), `window.rs` (`term_bg`, `Surroundings.terminal`), `find/run.rs:67`, `tiles.rs` (per-pane background and dim), `watch.rs` (`Changed.schemes`, watch `schemes/`), `restore.rs`

**Interfaces:**
- Consumes: Task 1 `SchemeSet`, `Scheme`, `From<&SchemeColors> for ColorScheme`; Task 2 fields.
- Produces:
  - `App::scheme_for(&self, pane: &Pane) -> &Scheme`: `pane.scheme` if it resolves, else `settings.color_scheme` if it resolves, else `graphite`.
  - `App::pane_style` takes colors from `scheme_for`, font family from `scheme.font_family` else settings, base size from `scheme.font_size` else `settings.font_size`, then today's zoom rule (zoom 0 keeps the base size unclamped; zoomed panes clamp to 6..48).
  - `App::reapply_schemes(&mut self, affected: impl Fn(&Pane) -> bool) -> Task<Message>`: for every affected pane, `ColorScheme::from(..).apply_to(&mut terminal.lock())`; redraw once; a font change resizes through the widget's re-measure and the existing debounced `pane_resized` path (no extra resize code).
  - Triggers: global setting change (all panes without an override), profile save (that profile's panes), scheme file change in `schemes/` from disk or the editor's Save (panes using that id, directly or via global).
  - New terminals get their pane's scheme at creation (local and SSH, fresh and restarted).
  - Background: `Tiles` paints each visible pane's scheme background (at `settings.background_opacity`) under the pane; the window container keeps painting the focused pane's background for padding; the dim overlay uses each pane's own background; the find match tint blends over the pane's background.
  - `Changed { profiles, settings, schemes }` (watch `schemes/` too; update literal constructions); `Message::ConfigChanged` reloads the set and reapplies.
  - Unknown ids: the settings notice says `Color scheme "{id}" not found for profile "{profile}"; using the global scheme.` or `Color scheme "{id}" not found; using Graphite.` (once per load).

- [ ] **Step 1: Write failing tests** (App-level, temp config dir): `default_panes_use_graphite_unchanged` (style equals today's), `global_setting_changes_all_unoverridden_panes`, `profile_override_wins`, `scheme_change_touches_only_its_panes`, `missing_profile_scheme_falls_back_with_notice`, `missing_global_falls_back_to_graphite`, `scheme_font_sets_pane_family_and_size` (zoom still relative), `saving_a_scheme_font_resizes_its_panes_once` (real `/bin/sh` pane under `cfg(unix)`: one session resize after the debounce), `osc_query_answers_with_pane_scheme`, `new_ssh_or_restarted_terminal_gets_pane_scheme`, `schemes_dir_change_reloads`.
- [ ] **Step 2: Run** — Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the gates, and an own-window screenshot of a split tab with two schemes (`FBTERM_SPLITS`, temp config with a profile override, neutral names) — Expected: PASS; single-pane default screenshot identical to before.
- [ ] **Step 5: Commit** `feat(schemes): apply schemes per pane`.

### Task 4: Choosing schemes in Settings and the profile editor

**Files:**
- Modify: `crates/fbterm/src/settings_window/{mod.rs, view.rs, glue.rs, tests.rs, app_tests.rs}`, `crates/fbterm/src/editor/{mod.rs, view.rs, tests.rs}`, `dialogs.rs`

**Interfaces:**
- Produces:
  - `SettingsMsg::ColorScheme(String)` (writes `SettingChange::ColorScheme` at once), `SettingsMsg::EditSchemes` (opens Task 5's window); an `Appearance` row `Color scheme` with a `pick_list` of scheme names (built-ins first) and an `Edit schemes…` button.
  - `EditorMsg::ColorScheme(Option<String>)`; `Draft.color_scheme: Option<String>`; a `Color scheme` pick_list in the profile editor's General section, first entry `Use global scheme` (None).
  - Both lists refresh when the scheme set changes.

- [ ] **Step 1: Write failing tests**: `settings_scheme_choice_writes_and_applies`, `settings_lists_builtins_first`, `editor_scheme_choice_saves_with_profile`, `editor_use_global_clears_override`, `lists_refresh_after_scheme_set_changes`.
- [ ] **Step 2: Run** — Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the gates; screenshots of both windows (`FBTERM_OPEN_SETTINGS=1`, `FBTERM_OPEN_EDITOR=<profile>`, `FBTERM_SCREENSHOT_WINDOW`) — Expected: PASS.
- [ ] **Step 5: Commit** `feat(schemes): pick schemes in Settings and the profile editor`.

### Task 5: Scheme editor window

**Files:**
- Create: `crates/fbterm/src/schemes_window/{mod.rs, view.rs, tests.rs}`
- Modify: `app.rs` (field, messages, title, `WindowClosed`, exit check), `window.rs` (routing), `windows.rs` (`quit`, `close_requested`), `actions.rs`, `palette.rs`, `crates/fbterm-config/src/keys.rs` (`Action::EditSchemes`, config name `edit_schemes`, no default chord, title `Color schemes`), `debug.rs` and `main.rs` (`FBTERM_OPEN_SCHEMES=1`, `FBTERM_SCREENSHOT_WINDOW=schemes`), `CLAUDE.md` debug aids

**Interfaces:**
- Consumes: Task 1 store functions; Task 3 `reapply_schemes`.
- Produces:
  - `App.schemes_win: Option<(window::Id, SchemesState)>`; `Message::Schemes(SchemesMsg)`.
  - `SchemesMsg::{Open, Close, Select(String), Duplicate, Rename(String), Delete, ConfirmDelete, CancelDelete, Color(Entry, Rgb), Hex(Entry, String), FontFamily(Option<FontChoice>), FontSize(Option<f32>), Save, Import, Imported(Option<PathBuf>), ImportChoice(ImportChoice), Export, Exported(Option<PathBuf>)}` with `enum Entry { Foreground, Background, Cursor, Selection, Ansi(u8) }` (Import/Export are Task 6; declare them here, inert).
  - Layout: left list (built-ins marked `Built-in`, read-only), right form for the selected scheme: name, a row per entry with a swatch, a hex `text_input` (validated with `Rgb::from_str`) and R/G/B sliders shown for the entry being edited, optional font family (`pick_list` with `Use global font`) and size, the contrast hint, and a live preview: a real `terminal_view` over a `SharedTerminal` fed fixed sample output (a prompt, `ls`-style colored names, all 16 colors in two rows, a selected word) drawn with the draft scheme.
  - Built-ins: form read-only; `Duplicate` creates `"{name} copy"` as a user scheme with a fresh id and selects it.
  - `Save` writes the file (Task 1 `save_user`) and calls `reapply_schemes` for panes using the id; unsaved edits change only the preview.
  - `Delete` asks with the exact copy in Global Constraints (profile count from `App.profiles`), deletes the file, clears those profiles' `color_scheme` (saving `profiles.toml`), and points the global setting back to `graphite` if it used the deleted id.
  - Opened from Settings (`Edit schemes…`), the palette (`Color schemes`) and the profile editor (a small `Edit schemes…` link next to the field).

- [ ] **Step 1: Write failing tests** (pure state + App-level): `builtins_are_read_only`, `duplicate_makes_editable_copy`, `hex_input_validates`, `edits_preview_without_touching_sessions`, `save_writes_and_reapplies`, `rename_keeps_references` (profiles still resolve after rename), `delete_asks_with_profile_count`, `delete_resets_profiles_and_global`, `contrast_hint_shows_for_low_contrast`, `window_opens_from_settings_palette_and_editor`.
- [ ] **Step 2: Run** — Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the gates; own-window screenshot via `FBTERM_OPEN_SCHEMES=1 FBTERM_SCREENSHOT_WINDOW=schemes` — Expected: PASS.
- [ ] **Step 5: Commit** `feat(schemes): color scheme editor with live preview`.

### Task 6: Import and export

**Files:**
- Modify: `crates/fbterm/src/schemes_window/{mod.rs, view.rs, tests.rs}`, `app.rs`

**Interfaces:**
- Consumes: Task 1 `read_file`, `write_file`, `save_user`, `new_user_id`; `rfd` save/pick dialogs parented to the schemes window (`window::run(id, |w| dialog.set_parent(w))`, filter `toml`), like `export.rs`.
- Produces: `Export…` writes the selected scheme (built-ins too) to a chosen path; `Import…` reads a file, validates it (errors shown in the window with file and line), and copies it into `schemes/`. A file with a built-in id gets a fresh user id. A file whose id exists asks `Replace` / `Keep both` / `Cancel` (`enum ImportChoice`); `Keep both` gets a fresh id and name `"{name} (imported)"`.

- [ ] **Step 1: Write failing tests**: `export_writes_a_loadable_file`, `import_copies_into_schemes_dir`, `import_invalid_file_shows_error`, `import_builtin_id_gets_new_id`, `import_same_id_keeps_both_with_new_id`, `import_same_id_replace_updates_and_reapplies`.
- [ ] **Step 2: Run** — Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the gates — Expected: PASS.
- [ ] **Step 5: Commit** `feat(schemes): import and export scheme files`.

### Task 7: Pane accent borders

**Files:**
- Modify: `crates/fbterm/src/tiles.rs`, `window.rs` (pass per-pane accents), `tiles.rs` tests

**Interfaces:**
- Produces: `tiles()` receives each pane's accent (`Option<Color>`). When the tab shows more than one pane: a pane with an accent gets a 1 px border in its accent (drawn in the divider gap like the focus outline); the focused pane's outline is drawn in its accent when it has one, else the theme accent as today; a pane without an accent gets no border. Single-pane tabs: unchanged (early return).

- [ ] **Step 1: Write failing tests**: `accent_border_only_in_split_tabs`, `focused_outline_uses_pane_accent`, `pane_without_accent_has_no_border` (headless `iced_tiny_skia` draw, or a pure helper returning the rects and colors to draw).
- [ ] **Step 2: Run** — Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the gates; screenshot of a split tab with two accented profiles — Expected: PASS.
- [ ] **Step 5: Commit** `feat(tiling): profile accents on pane borders`.

### Task 8: Docs

**Files:**
- Create: `website/docs/color-schemes.md` (sidebar entry after `tiling`), `website/static/img/color-schemes.png`
- Modify: `website/docs/files.md` (`schemes/` folder, `color_scheme` key), `profiles.mdx` (`color_scheme` field), `tiling.md` (pane accent borders), `keyboard-shortcuts.md` (`edit_schemes` action), main spec `docs/superpowers/specs/2026-10-23-featherbit-terminal-design.md` 5.10 and 6 (milestone 2 done, dated), `CLAUDE.md` (Status; a gotcha line on scheme resolution and `reapply_schemes`)

- [ ] **Step 1: Write** the docs page (what a scheme is, global vs profile, built-ins, the editor, fonts in schemes, files and sharing, the file format with an example) and the other edits. Screenshot of the editor window with sample data only (no real user or host names).
- [ ] **Step 2: Run** `cd website && npm run build` and the gates — Expected: PASS.
- [ ] **Step 3: Commit** `docs(schemes): color schemes guide`.
