# Theme Gallery Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A `featherbitplatform/terminal-themes` repo with a validated catalog of permissively licensed, credited themes, and a gallery in fbterm's Color schemes window that browses, installs and updates them.

**Architecture:** `terminal-themes` holds one TOML file per theme and a generated `catalog.toml` that its CI keeps valid and current; merging to `main` publishes. fbterm parses the catalog (untrusted) in `fbterm-config`, fetches it through the shared HTTPS helper `crates/fbterm/src/net.rs` only on user action, and installs themes as ordinary user scheme files that also carry `[credit]` and `[gallery]` tables.

**Tech Stack:** Rust (fbterm workspace, iced 0.14, serde/toml), Python 3.11+ standard library (`tomllib`) for the catalog tool and its tests, GitHub Actions.

**Spec:** `docs/superpowers/specs/2026-10-28-theme-gallery-design.md` (builds on `2026-10-27-color-schemes-design.md`).

## Global Constraints

- License allowlist (exact SPDX ids): `MIT`, `Apache-2.0`, `BSD-2-Clause`, `BSD-3-Clause`, `ISC`, `Unlicense`, `CC0-1.0`, `Zlib`.
- Theme id: lowercase slug `^[a-z0-9]+(-[a-z0-9]+)*$`, equal to the file name, never reused; installed scheme id is `gallery-<id>`.
- Catalog URL (fixed): `https://raw.githubusercontent.com/featherbitplatform/terminal-themes/main/catalog.toml`; catalog `format = 1`.
- Fetch: HTTPS only, certificate verification, size cap 512 KiB, timeout 15 s, off the UI thread, only on opening the gallery or pressing Refresh, Install or Update; no disk cache; never triggered by a remote program.
- An unknown higher catalog `format` shows `This catalog needs a newer fbterm.`
- fbterm makes no other new network request. No new crate besides what `net.rs` (from `feature/update-check`) already brought.
- Older fbterm versions must keep loading gallery scheme files (the current `SchemeFile` has no `deny_unknown_fields`; keep it that way).
- Never name or compare to another terminal application in either repo (credits link to each theme's own main project). Product copy: no emoji, no em-dashes.
- Commits: Conventional Commits, no attribution lines of any kind.
- Running fbterm: temp `FBTERM_CONFIG_DIR`, `FBTERM_SECRETS=memory`, hard timeout; screenshots only through `FBTERM_SCREENSHOT`.

## Review Focus

- The catalog URL answers 200 with something that is not TOML (an HTML error page, a truncated body): the gallery shows its error line with Retry; nothing is installed or changed. Test in Task 4 (`html_body_is_an_error`) and Task 5 (`bad_catalog_shows_error_and_keeps_installed`).
- A user already has a hand-made `gallery-<id>.toml` (or one saved by an older fbterm, without `[gallery]`): the gallery treats it as installed and edited, Update asks first, the id and the profiles using it survive. Test in Task 3 (`gallery_id_without_block_counts_as_edited`) and Task 5 (`update_of_blockless_file_asks`).
- Update of a theme in use by open panes: the file is replaced in place under the same id and those panes recolor. Test in Task 5 (`update_in_place_recolors_open_panes`).
- A catalog over 512 KiB or a theme entry with a huge string: refused by the size cap or skipped by validation, never written. Test in Task 4 (`oversized_fields_are_skipped`).
- A catalog theme whose `credit.url` or link is not `https://`: skipped, so the gallery never opens a non-HTTPS link. Test in Task 4 (`non_https_credit_url_is_skipped`).

---

## Part A: `terminal-themes` (repo at `../terminal-themes`, empty, default branch to be `main`)

### Task 1: Catalog tool, CI and the first theme

**Files:**
- Create: `README.md`, `LICENSE` (Apache-2.0 text), `.gitignore`
- Create: `tools/catalog.py` (validate + generate), `tools/test_catalog.py` (unittest)
- Create: `themes/catppuccin-mocha.toml`, `LICENSES/catppuccin-MIT.txt`
- Create: `catalog.toml` (generated), `.github/workflows/ci.yml`

**Interfaces:**
- Produces: theme file schema (spec section 4) and `catalog.toml`: top-level `format = 1`, then `[[themes]]` entries each holding every theme-file field plus optional `withdrawn = true`. `python3 tools/catalog.py check` (validate + fail if `catalog.toml` or README table is stale), `python3 tools/catalog.py build` (rewrite both), `python3 tools/catalog.py versions <base-ref>` (fail if any id disappeared or any version did not grow when its file changed).

- [ ] **Step 1: Write failing tests** in `tools/test_catalog.py`: `test_valid_theme_passes`, `test_license_not_on_allowlist_fails` (`GPL-3.0-only`), `test_missing_credit_field_fails` (each of project, author, url, license, copyright, license_file, description), `test_license_file_must_exist`, `test_url_must_be_https`, `test_id_must_match_file_and_slug`, `test_bad_color_fails` (`#12345`, `red`), `test_build_is_deterministic` (sorted by id, byte-identical twice), `test_removed_id_fails_versions`, `test_changed_theme_without_version_bump_fails`, `test_contrast_is_reported_not_fatal` (fg/bg 1.5:1 passes `check` with a warning line naming the theme).
- [ ] **Step 2: Run** `python3 -m unittest discover -s tools` and see them fail.
- [ ] **Step 3: Implement `tools/catalog.py`** with `tomllib` only (no third-party packages). Contrast as WCAG relative luminance, warning below 4.5:1 for foreground on background. The README theme table (name, variant, project link, author, license) is generated between `<!-- themes:start -->` and `<!-- themes:end -->`.
- [ ] **Step 4: Add Catppuccin Mocha** from the official palette (re-read `https://github.com/catppuccin/catppuccin/blob/main/LICENSE`; copy its text into `LICENSES/catppuccin-MIT.txt`), `version = 1`, `derived = false`. Run `python3 tools/catalog.py build`.
- [ ] **Step 5: CI** `.github/workflows/ci.yml` on pull_request and push to `main`: set up Python 3.12, run the unit tests, `catalog.py check`, and on pull requests `catalog.py versions origin/${{ github.base_ref }}` (fetch-depth 0).
- [ ] **Step 6: Run** tests and `check`; expect PASS. README states: one file per theme, permissive licenses only (the allowlist), credits required, merging to `main` publishes, and the never-name-other-terminals rule for contributors.
- [ ] **Step 7: Commit** on branch `init` (`feat: catalog tool, CI and Catppuccin Mocha`), push, open a PR to `main` (the empty repo's first push of `main` is the owner's merge; if GitHub needs a base branch, push an initial README-only commit to `main` first and say so in the report).

### Task 2: The first catalog

**Files:**
- Create: `themes/<id>.toml` for every theme in spec section 7a, `LICENSES/<project>-<SPDX>.txt` per upstream project (and NOTICE text where an Apache-2.0 source has one)
- Create: `previews/derived.svg` (one swatch row per `derived = true` theme: background, foreground, cursor, selection, 16 colors, theme name), generated by `tools/catalog.py previews`
- Modify: `catalog.toml`, `README.md` (regenerated)

**Interfaces:**
- Consumes: Task 1's schema and tool.
- Produces: ids `catppuccin-latte`, `catppuccin-frappe`, `catppuccin-macchiato`, `catppuccin-mocha`, `gruvbox-dark`, `gruvbox-light`, `nord`, `one-dark`, `one-light`, `vscode-dark-modern`, `vscode-dark-plus`, `vscode-light-modern`, `vscode-light-plus`, `dracula`, `solarized-dark`, `solarized-light`, `tokyo-night`, `tokyo-night-storm`, `tokyo-night-day`, `rose-pine`, `rose-pine-moon`, `rose-pine-dawn`, `everforest-dark`, `everforest-light`, `kanagawa`, `ayu-dark`, `ayu-mirage`, `ayu-light`, `github-dark`, `github-light`.

- [ ] **Step 1: Re-read each upstream LICENSE** (spec 7a sources) and resolve the *verify* items: Nord name-use terms, VS Code `extensions/theme-defaults` notices, Primer license files. A theme that fails or cannot be resolved is left out and listed in the report with the reason.
- [ ] **Step 2: Write each theme** from the project's own published colors (official terminal colors where the project publishes them; otherwise derive the 16 colors, cursor and selection from its palette, set `derived = true`, and say how in `description`). `copyright` is the upstream notice verbatim. Gruvbox credits Pavel Pertsev (morhetz) and cites the gruvbox-community LICENSE file; Tokyo Night Storm and Day credit Enkia and folke's tokyonight.nvim (Apache-2.0, notice kept).
- [ ] **Step 3: Add `previews` to the tool** (SVG written with the standard library) and generate `previews/derived.svg`.
- [ ] **Step 4: Run** `python3 -m unittest discover -s tools` and `python3 tools/catalog.py check`; expect PASS (contrast warnings allowed, listed in the report).
- [ ] **Step 5: Commit** (`feat: first catalog`) on branch `first-catalog`, push, open a PR to `main`. **Stop for the owner:** the report links `previews/derived.svg` and the excluded list; the owner approves before merge (merging publishes).

## Part B: fbterm (worktree of `feature/theme-gallery`)

Before Task 3: `git merge origin/develop` once `feature/update-check` (which adds `crates/fbterm/src/net.rs` and a browser opener) has merged; if it has not, stop and say so.

### Task 3: Credit and gallery tables on schemes

**Files:**
- Modify: `crates/fbterm-config/src/schemes.rs`
- Test: same file's tests module

**Interfaces:**
- Produces:
  - `pub struct Credit { pub project: String, pub author: String, pub url: String, pub license: String, pub copyright: String, pub license_file: String, pub description: String, pub derived: bool }` (serde, all required when the table is present).
  - `pub struct GalleryInfo { pub id: String, pub version: u32, pub colors_hash: String }`.
  - `Scheme` gains `pub credit: Option<Credit>` and `pub gallery: Option<GalleryInfo>`, written as `[credit]` and `[gallery]` tables, omitted when `None`.
  - `pub const GALLERY_PREFIX: &str = "gallery-";`
  - `pub fn colors_hash(colors: &SchemeColors) -> String` (stable across runs and platforms: FNV-1a 64 over the 20 colors in file order, lowercase hex; no new crate).
  - `pub fn gallery_id(scheme: &Scheme) -> Option<&str>` (the catalog id from a `gallery-` scheme id, with or without `[gallery]`).
  - `pub fn is_edited(scheme: &Scheme) -> bool` (`gallery` is `None`, or its `colors_hash` differs from `colors_hash(&scheme.colors)`).

- [ ] **Step 1: Failing tests:** `credit_and_gallery_round_trip`, `old_files_without_tables_load`, `file_with_tables_loads_in_old_shape` (deserialize into a struct shaped like today's `SchemeFile` and check it still parses: proves older fbterm compatibility), `colors_hash_is_stable` (pin the hex for the Graphite colors), `gallery_id_without_block_counts_as_edited`, `edited_colors_count_as_edited`, `untouched_gallery_scheme_is_not_edited`, `is_valid_user_accepts_gallery_ids`.
- [ ] **Step 2: Run** `cargo test -p fbterm-config schemes`; FAIL.
- [ ] **Step 3: Implement.** Update every `Scheme { .. }` construction in the workspace (built-ins and tests get `None`); duplicating or importing a scheme keeps `credit`, and Duplicate clears `gallery` (a copy is the user's own scheme).
- [ ] **Step 4: Run** `cargo test --workspace`; PASS.
- [ ] **Step 5: Commit** `feat(schemes): credit and gallery tables in scheme files`.

### Task 4: Catalog parsing and install status

**Files:**
- Create: `crates/fbterm-config/src/catalog.rs` (+ `pub mod catalog;` in `lib.rs`)
- Test: same file

**Interfaces:**
- Consumes: Task 3 types.
- Produces:
  - `pub const CATALOG_URL: &str = "https://raw.githubusercontent.com/featherbitplatform/terminal-themes/main/catalog.toml";`, `pub const CATALOG_FORMAT: u32 = 1;`, `pub const MAX_CATALOG_BYTES: usize = 512 * 1024;`, `pub const LICENSES: [&str; 8]` (the allowlist).
  - `pub struct CatalogTheme { pub id: String, pub version: u32, pub dark: bool, pub withdrawn: bool, pub scheme: Scheme /* id gallery-<id>, credit and gallery set */ }`
  - `pub struct Catalog { pub themes: Vec<CatalogTheme>, pub skipped: Vec<String> /* "<id or index>: reason" */ }`
  - `pub enum CatalogError { NotToml(String), NewerFormat(u32), Empty }`
  - `pub fn parse_catalog(bytes: &[u8]) -> Result<Catalog, CatalogError>`
  - `pub enum Status { Available, Installed, Update { edited: bool }, Withdrawn { installed: bool } }`
  - `pub fn status(theme: &CatalogTheme, installed: Option<&Scheme>) -> Status`

- [ ] **Step 1: Failing tests:** `parses_a_valid_catalog`, `html_body_is_an_error` (`<!DOCTYPE html>...` gives `NotToml`), `newer_format_is_reported` (`format = 2`), `invalid_theme_is_skipped_not_fatal` (bad color in one of two themes: one kept, one in `skipped`), `license_off_allowlist_is_skipped`, `missing_credit_is_skipped`, `non_https_credit_url_is_skipped`, `bad_id_is_skipped` (`../x`, `Upper`, `gallery-x`), `oversized_fields_are_skipped` (a 10 000-character name or description), `duplicate_ids_keep_the_first`, `status_cases` (not installed: Available; same version: Installed; higher catalog version untouched: `Update { edited: false }`; edited: `Update { edited: true }`; blockless `gallery-` file at any version: `Update { edited: true }`; withdrawn: `Withdrawn`).
- [ ] **Step 2: Run** `cargo test -p fbterm-config catalog`; FAIL.
- [ ] **Step 3: Implement** with serde structs mirroring Task 1's catalog shape (unknown keys ignored so the catalog can grow); each theme validated through the same checks as a scheme file (`is_valid_user` on the built `Scheme`), plus credit completeness, allowlist, `https://` url, slug id, string fields at most 2 000 characters.
- [ ] **Step 4: Run** `cargo test -p fbterm-config`; PASS.
- [ ] **Step 5: Commit** `feat(schemes): parse the theme catalog`.

### Task 5: Gallery state, fetch, install and update

**Files:**
- Create: `crates/fbterm/src/schemes_window/gallery.rs` (state and App glue)
- Modify: `crates/fbterm/src/schemes_window/mod.rs` (`SchemesMsg` variants, state field), `glue.rs` (dispatch)
- Test: `crates/fbterm/src/schemes_window/gallery_tests.rs`

**Interfaces:**
- Consumes: `net.rs` from `feature/update-check` (read its signature there; call it with `CATALOG_URL`, `MAX_CATALOG_BYTES`); Task 4's `parse_catalog`, `status`; `save_user`, `App::reload_schemes`.
- Produces:
  - `pub enum GalleryLoad { Idle, Loading, Loaded(Catalog), Failed(String) }` on the Color schemes state (`gallery: GalleryLoad`, `showing_gallery: bool`).
  - `SchemesMsg::{ShowGallery(bool), RefreshGallery, GalleryFetched(Result<Vec<u8>, String>), Install(String), Update(String), UpdateChoice(UpdateChoice), OpenCreditLink(String)}` with `pub enum UpdateChoice { Replace, Copy, Cancel }`; the edited-update question lives in state as `pending_update: Option<String>`.
  - The fetch goes through an injectable function on `App` (a `fn(&str, usize) -> Result<Vec<u8>, String>` field or trait object, defaulting to `net.rs`), so tests never touch the network.

- [ ] **Step 1: Failing tests** (fake fetch): `opening_gallery_fetches_once` (opening twice while Loading fetches once; Refresh fetches again), `nothing_fetches_without_the_gallery` (opening the Color schemes window alone makes no fetch), `bad_catalog_shows_error_and_keeps_installed`, `offline_shows_error_line` (the line `Couldn't load the theme gallery: <reason>`), `install_writes_gallery_file` (file `gallery-<id>.toml` with `[credit]` and `[gallery]`, scheme selectable), `update_in_place_recolors_open_panes` (a pane using `gallery-x` shows the new background after Update), `update_of_edited_asks` then `Replace` overwrites and `Copy` makes a `user-<uuid>` scheme keeping `[credit]` with no `[gallery]`, `update_of_blockless_file_asks`, `withdrawn_installed_stays`, `newer_format_message` (`This catalog needs a newer fbterm.`), `credit_link_only_https` (OpenCreditLink with a non-https string opens nothing).
- [ ] **Step 2: Run** `cargo test -p fbterm gallery`; FAIL.
- [ ] **Step 3: Implement.** Fetch on the blocking pool, result back as `GalleryFetched`; catalog kept in state for the app session; Install/Update write through `save_user` then `reload_schemes` and `schemes_moved` so panes recolor; Update replaces only after the Replace answer when edited.
- [ ] **Step 4: Run** `cargo test --workspace`, clippy; PASS.
- [ ] **Step 5: Commit** `feat(schemes): install and update themes from the gallery`.

### Task 6: Gallery view and credits in the Color schemes window

**Files:**
- Modify: `crates/fbterm/src/schemes_window/view.rs`, `crates/fbterm/src/debug.rs` (or wherever `FBTERM_OPEN_SCHEMES` is read)
- Test: view-model tests in `gallery_tests.rs`

**Interfaces:**
- Consumes: Task 5 state and messages.
- Produces: debug aid `FBTERM_DEMO_GALLERY=1` (debug builds only): the gallery shows a built-in fixture catalog of three themes (one installed, one with an update, one withdrawn) without fetching, for screenshots.

- [ ] **Step 1: Failing tests** on a pure row model: `pub fn gallery_rows(catalog: &Catalog, installed: &SchemeSet, filter: &str, variant: Option<bool>) -> Vec<GalleryRow>` with `GalleryRow { id, name, credit_line /* "<project> by <author>, <license>" */, button: Option<RowButton /* Install | Installed | Update */>, note: Option<&'static str /* "No longer offered" */>, colors: SchemeColors }`: filter by name or project text, dark/light filter, withdrawn not installed hidden, withdrawn installed shown with the note.
- [ ] **Step 2: Run**; FAIL.
- [ ] **Step 3: Implement** the view: a `Gallery` switch beside the scheme list; each row shows the preview strip (background, foreground, 16 colors), name, credit line, a link button to `credit.url` (`OpenCreditLink`), the row button; the loading, error (with `Retry`) and newer-format states. The scheme detail pane shows, for a scheme with `credit`, `<project> by <author>, <license>` and its link, offline. Follow the Graphite design tokens used elsewhere in the window.
- [ ] **Step 4: Run** tests and take a screenshot: `FBTERM_CONFIG_DIR=$(mktemp -d) FBTERM_SECRETS=memory FBTERM_OPEN_SCHEMES=1 FBTERM_DEMO_GALLERY=1 FBTERM_SCREENSHOT_WINDOW=schemes FBTERM_SCREENSHOT=<scratch>/gallery.png perl -e 'alarm 60; exec @ARGV' target/debug/fbterm`; check the rows render.
- [ ] **Step 5: Commit** `feat(schemes): gallery view and theme credits`.

### Task 7: Docs

**Files:**
- Modify: the website's color schemes page (`website/docs/`, find it), `LICENSES.md`, `CLAUDE.md`

- [ ] **Step 1:** Website: the gallery (where, Install, Update and the edited-theme question, credits), what it contacts (the one catalog URL on GitHub) and only when, offline behaviour, link to `terminal-themes` for full credits and how to propose a theme. `LICENSES.md`: gallery themes are not part of fbterm; each carries its own license and credit in its file and in `terminal-themes`. `CLAUDE.md`: a gotcha on the gallery (catalog untrusted, fetch only on user action through `net.rs`, `gallery-` ids, `[credit]`/`[gallery]` tables) and the `FBTERM_DEMO_GALLERY` debug aid; Status line.
- [ ] **Step 2: Run** `cd website && npm ci && npm run build` (fails on broken links); PASS.
- [ ] **Step 3: Commit** `docs: theme gallery`.
