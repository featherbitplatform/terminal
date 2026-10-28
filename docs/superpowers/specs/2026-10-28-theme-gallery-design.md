# Theme gallery: design

Status: draft for the owner's review, 2026-10-28. Builds on
`2026-10-27-color-schemes-design.md` (schemes, scheme files, the Color schemes window), which
stays binding for everything this document does not change.

## 1. Intent

The owner wants many more color schemes, inspired by popular palettes (Gruvbox, Catppuccin,
Nord, One Dark, One Light, the VS Code defaults and others), without an fbterm release for each
new one. Themes live in a separate repository, `featherbitplatform/terminal-themes`, and users
browse and install them from inside fbterm. Only themes under permissive licenses are accepted,
and every theme credits its original author and project, with its license, wherever it is shown.

Success: the owner opens the Color schemes window, browses the gallery, sees Catppuccin Mocha
with a preview, its author, license and project link, installs it with one click and picks it
for a profile. A month later a color fix is merged in `terminal-themes`; the next time the owner
opens the gallery, Catppuccin Mocha shows **Update**.

## 2. Goals

1. A new repository, `featherbitplatform/terminal-themes`, holds one file per theme in fbterm's
   scheme format plus a credit block, and publishes a catalog of all of them.
2. The Color schemes window gets a **gallery** that loads the catalog over HTTPS, previews each
   theme, shows its credit, and **installs** it into the user's `schemes/` folder.
3. An installed theme whose catalog version is newer shows an **Update** button in the gallery.
4. Every installed theme keeps its credit; the Color schemes window shows it, offline included.
5. The first catalog has every theme that passed the license research (section 7).

## 3. Non-goals and anti-goals

- No background network access: no check at startup, no periodic refresh, no automatic updates.
- No user accounts, ratings, uploads or submitting themes from the app; contributions are pull
  requests to `terminal-themes`.
- No other catalog sources (custom URLs, mirrors) in this version.
- The four built-in schemes stay built in and unchanged; the gallery does not replace them.
- Anti-goals, a failure even if the gallery works:
  - A theme in the catalog without a permissive license or without complete credit.
  - Any network access the user did not cause by opening the gallery or pressing Refresh,
    Install or Update.
  - A malformed or hostile catalog crashing the app, writing outside `schemes/`, or installing
    something that is not a valid scheme.
  - An Update silently overwriting a theme the user edited.
  - A remote program (anything running in a terminal) causing a gallery fetch or install.
  - Naming or comparing to other terminal applications, in either repository (credits link to
    the theme's own main project, never to another terminal's port of it).

## 4. The `terminal-themes` repository

**Layout.**
- `themes/<id>.toml`: one theme. `<id>` is a lowercase slug (`catppuccin-mocha`), unique and
  fixed forever once published.
- `catalog.toml`: generated from `themes/` by a script in the repo and committed; what the app
  downloads. CI fails if it is not up to date.
- `LICENSES/`: the full license text of every upstream project a theme comes from, one file per
  project, named so a theme's `[credit]` can point to it.
- `README.md`: what the repo is, how to add a theme, and a table of every theme with its credit
  (generated with the catalog).
- `LICENSE`: the repo's own license for its scripts and docs (Apache-2.0, like fbterm). Each
  theme's colors are under its upstream license, stated in its file.

**A theme file** is an fbterm scheme file (same `name`, optional font keys and `[colors]` table
as the color schemes design) plus:

```toml
id = "catppuccin-mocha"        # the slug, same as the file name
version = 1                    # integer, raised on every change to the theme
variant = "dark"               # "dark" or "light", for grouping in the gallery

[credit]
project = "Catppuccin"
author = "Catppuccin"                               # as the upstream copyright line names it
url = "https://github.com/catppuccin/catppuccin"    # the theme's main project
license = "MIT"                                     # SPDX id
copyright = "Copyright (c) 2021 Catppuccin"         # upstream notice, verbatim
license_file = "LICENSES/catppuccin-MIT.txt"
description = "Soothing pastel theme, Mocha flavor."
derived = false   # true when the 16 terminal colors are fbterm's own mapping of a palette
```

**Rules CI enforces** (a theme cannot be merged otherwise):
- `license` is on an allowlist: MIT, Apache-2.0, BSD-2-Clause, BSD-3-Clause, ISC, Unlicense,
  CC0-1.0, Zlib.
- Every `[credit]` field is present and non-empty; `license_file` exists; `url` is `https://`.
- The theme parses as a valid fbterm scheme (all colors `#rrggbb`, non-empty name).
- `id` matches the file name and the slug format; ids never disappear from the catalog once
  published (a withdrawn theme is marked, not removed; see section 5).
- `version` only grows (CI compares with the base branch).
- Apache-2.0 sources: their NOTICE text, if any, is kept in `LICENSES/`.

**The catalog** (`catalog.toml`) holds a format version and every theme in full (all fields
above), so one download is enough to list, preview and install. Size stays well under the
app's limit (section 5).

## 5. The gallery in fbterm

**Where.** The Color schemes window gets a **Gallery** view next to the list of the user's
schemes (exact control is the builder's: a tab, a segmented switch or a button). Opening it
loads the catalog; a **Refresh** control loads it again. Nothing else fetches.

**Fetching.**
- One fixed URL, compiled in: the raw `catalog.toml` on `main` of
  `featherbitplatform/terminal-themes` over HTTPS
  (`https://raw.githubusercontent.com/featherbitplatform/terminal-themes/main/catalog.toml`).
- HTTPS with certificate verification; no redirects to other hosts; a size limit (512 KiB) and
  a timeout (15 s); done off the UI thread.
- The catalog is untrusted input: parsed strictly, each theme validated like an imported scheme
  file, plus the credit and `id` rules of section 4. Invalid themes are skipped (logged), not
  fatal; an unknown future format version shows "This catalog needs a newer fbterm".
- The last good catalog is kept in memory for the app session only (no disk cache).
- Errors (offline, timeout, bad response) show one plain line in the gallery with Retry;
  installed themes are unaffected.

**Listing.** Each theme shows its name, a preview (background, foreground and the 16 colors,
drawn from the catalog data; no extra download), its variant, and its credit: project, author,
license and a link to the project page (opened in the system browser). Filtering by text and by
dark/light is the builder's call.

**Installing.**
- **Install** writes `schemes/gallery-<id>.toml`: the scheme with stable scheme id
  `gallery-<id>`, plus the `[credit]` block and a `[gallery]` block recording `id`, `version`
  and a hash of the scheme as installed (name, font and colors). It is then a normal user scheme: selectable globally
  and per profile, exportable, editable.
- Installing an already installed theme at the same version is not offered (the button reads
  **Installed**).
- `gallery-` ids are lenient: a scheme file with a `gallery-<id>` id but no `[gallery]` block
  (for example saved by an older fbterm, which drops tables it does not know) keeps its id, so
  the profiles using it keep working. The gallery matches it to the catalog by id, treats it as
  edited (section "Updating"), and the Color schemes window shows the catalog's credit for it
  while the catalog is loaded; installing or updating writes the blocks back.

**Updating.**
- When the catalog version of an installed theme is higher than the installed one, its row
  shows **Update**.
- If the installed name, font and colors still match the recorded hash, Update replaces the file in place
  (same scheme id, so profiles using it keep it and recolor).
- If the user edited it, Update asks: **Replace my changes**, **Install as a copy** (a new
  user scheme with the new colors and the credit; the edited one stays) or **Cancel**.
- A theme withdrawn from the catalog (marked so in `terminal-themes`) is never removed from a
  user's machine; its row says it is no longer offered.

**Credit after install.** The Color schemes window shows, for a scheme with a `[credit]`
block, the project, author, license and link, offline. Exports (a scheme file or a
profile archive) keep the `[credit]` block only, so a shared copy keeps its attribution; the
`[gallery]` block is local bookkeeping. Imports (a scheme file or an archive) drop any
`[gallery]` block, so an imported copy of a gallery theme counts as edited and Update asks. Editing a gallery theme keeps its
credit (the colors stay derived from the original).

**Docs.** The website's color schemes page explains the gallery, the network access it makes
and when, and links to `terminal-themes` for the full credits. `LICENSES.md` in the terminal
repo states that gallery themes are not part of the app and carry their own licenses.

## 6. Constraints

- fbterm makes no network request outside SSH except the gallery's, and only on user action.
- A new HTTP client dependency is allowed for this: rustls-based (no OpenSSL, so packaging on
  every platform stays as is), small, maintained, passing `cargo deny`. Builder's choice which.
- Scheme files keep loading in older fbterm versions: the new `[credit]` and `[gallery]` tables
  are additions; check how the current loader treats unknown tables and keep old versions
  working (if they reject unknown keys, the builder decides how to stay compatible and says
  so).
- Remote programs never trigger a fetch, install or update.
- Product copy follows the design system: no emoji, no em-dashes.

## 7. Building the first catalog

The builder does this work and reports to the owner:
- Each theme's upstream license file is re-read when the theme is added. A theme whose check
  fails, or whose *verify* item cannot be resolved, is left out of the catalog and listed for
  the owner; nothing is dropped silently.
- Upstream colors are kept as published. Where the project publishes no `cursor` or
  `selection` color, or no 16-color terminal mapping (`derived = true` themes), the builder
  derives them from the project's own palette, records it in the theme's `description`, and
  shows the owner a preview of every derived theme before it is published.
- Contrast is reported, not enforced: the `terminal-themes` CI prints each theme's foreground
  and background contrast and flags those under the built-ins' bar, but does not refuse them,
  because changing a well-known palette to pass would misrepresent it.

## 7a. The first catalog (owner-approved, 2026-10-28: every qualifying theme)

From the license research of 2026-10-28; each license file is to be re-read from the upstream
repository when the theme is added, and the items marked *verify* resolved first.

| Theme(s) | Credit | License | Notes |
|---|---|---|---|
| Catppuccin Latte, Frappé, Macchiato, Mocha | Catppuccin | MIT | official terminal colors |
| Gruvbox Dark, Gruvbox Light | Pavel Pertsev (morhetz) | MIT | original repo states MIT in README; LICENSE file in gruvbox-community; palette only, mapping ours |
| Nord | Sven Greb | MIT | official terminal colors; *verify* any name-use terms |
| One Dark, One Light | GitHub Inc. (Atom) | MIT | syntax colors only; terminal mapping ours (`derived = true`) |
| VS Code Dark Modern, Dark+, Light Modern, Light+ | Microsoft Corporation | MIT | terminal colors in the theme files; *verify* the theme-defaults files carry no other notice |
| Dracula | Dracula Theme | MIT | free version only, never Dracula Pro |
| Solarized Dark, Solarized Light | Ethan Schoonover | MIT | README defines the terminal mapping |
| Tokyo Night, Tokyo Night Storm, Tokyo Night Day | Enkia; Storm and Day terminal colors from folke's tokyonight.nvim | MIT; Apache-2.0 (folke) | keep folke's notice |
| Rosé Pine, Rosé Pine Moon, Rosé Pine Dawn | Rosé Pine | MIT | official terminal colors |
| Everforest (dark, light) | sainnhe | MIT | palette only, mapping ours |
| Kanagawa | Tommaso Laurenzi | MIT | official terminal colors |
| Ayu (dark, mirage, light) | Konstantin Pschera | MIT | palette only, mapping ours |
| GitHub (dark, light) | Primer | MIT | from github-vscode-theme; *verify* primer license files |

Excluded: City Lights (CC BY-NC-ND 4.0), Eclipse (EPL-2.0, and no terminal palette), Monokai
original (no license), Material Theme (license could not be confirmed).

## 8. Decided by the owner (2026-10-28)

- Themes in a separate repo, `featherbitplatform/terminal-themes`.
- Browse and install from inside fbterm (not bundled at build time).
- `terminal-themes` publishes on merge: pull requests go straight into `main`, gated by its CI;
  no `develop` branch and no tags. What is on `main` is what users see.
- An installed theme with a newer catalog version shows an **Update** button in the gallery.
- Only permissive licenses; every theme credits author, project link and license.
- The first catalog holds every theme that qualified.

## 9. Calls made without the owner (check these)

- Fetch only on opening the gallery or pressing Refresh; no disk cache of the catalog.
- One catalog file with every theme in full, from the raw GitHub URL of `main`.
- Edited themes: Update asks Replace / Install as a copy / Cancel.
- Installed themes get scheme ids `gallery-<id>` so updates keep profile references; a
  `gallery-` file without its `[gallery]` block keeps its id and counts as edited.
- The builder prepares the first catalog: failed or unverifiable themes are held for the owner,
  derived colors are previewed to the owner before publishing, contrast is reported, not
  enforced.
- Withdrawn themes stay installed; ids are never reused.
- `terminal-themes` is Apache-2.0 for its own scripts and docs, and follows fbterm's rule of
  never naming other terminal applications.

## 10. Left to the builder

The gallery's layout and controls, the preview's look, the HTTP client crate, the catalog
generator's language, the hash used to detect edits, how the reserved `gallery-` prefix is
enforced, and the exact copy of messages (owner-facing strings above are fixed).
