# Color schemes and pane accents: design

Status: draft for the owner's review, 2026-10-27. Completes milestone 2 of
`2026-10-23-featherbit-terminal-design.md` (sections 5.10 "Colors" and 6), which this document
refines. Where they differ, this document wins for color schemes and pane accents.

## 1. Intent

Different sessions should be able to look different, for whatever reason the user has:
labelling an environment (production in a color that warns), telling where a machine lives,
or just taste. There is one global look for every session, and any profile can override it.
Users pick from built-in schemes, make their own in a graphical editor with a live preview, and
share them as files.

Success: the owner gives the production profile a red-tinted scheme, opens it next to a local
shell in a split tab, and the two panes are unmistakably different at a glance, while the tab
bar and the rest of the app keep the app theme.

## 2. Goals

1. A **color scheme** is a named look for a session: foreground, background, cursor, selection
   and the 16 ANSI colors. A scheme may also set a **font family and size**; when it does not,
   the global font settings apply.
2. **Built-in schemes** ship with the app, the current Graphite scheme among them and still the
   default.
3. A **global scheme** applies to every session; **each profile can override it** with another
   scheme.
4. Users **create, duplicate, rename, edit and delete their own schemes** in a graphical editor
   with a live preview, and **export and import** them as files to share.
5. In a split tab each pane uses its own profile's scheme, and a profile's **accent** colors its
   pane's border, so hosts stay recognizable side by side.

## 3. Non-goals and anti-goals

- **A scheme never changes the app chrome.** Tab bar, dialogs, Settings, palette and editors
  keep the app's Dark/Light theme; a scheme rules only inside its sessions. (Owner's decision,
  2026-10-27.)
- No importing other applications' theme file formats in this version; the shareable format is
  fbterm's own (main design 5.10 lists third-party formats as a nice-to-have, still open).
- No per-tab or per-session ad-hoc scheme switching outside profiles in this version.
- Anti-goals, a failure even if schemes work:
  - Users who never touch schemes see exactly what they see today.
  - A missing or broken scheme file never stops a session from opening; it falls back to the
    global scheme with a visible notice.
  - Remote programs cannot change which scheme applies or write scheme files (they may still
    set colors for their own session through the escape sequences fbterm already honors).
  - Unreadable color combinations are not silently shipped as built-ins: every built-in meets
    the contrast bar the Graphite scheme is tested against.

## 4. Behaviour (owner-approved, 2026-10-27)

**Global and per-profile.**
- Settings gets a "Color scheme" choice (global). The profile editor gets a "Color scheme"
  choice whose first entry is "Use global scheme" (the default; not written to the file).
- A session uses its profile's scheme if set, otherwise the global one. Changing either applies
  to open sessions of the affected profiles when saved.
- The terminal answers color queries (OSC 10/11/4) with the scheme it is drawn with, per pane.

**Fonts in a scheme.** A scheme may set a font family, a font size, both, or neither. When set,
they replace the global font settings for sessions using that scheme. Per-pane zoom stays
relative to whatever size applies.

**Built-in schemes.** Read-only; they can be duplicated as the starting point of a user scheme.

**User schemes and sharing.**
- Stored as one TOML file per scheme in a `schemes/` folder inside the config directory, so a
  file can be copied, versioned or sent to someone as is.
- The editor offers Export (save a copy to a chosen path) and Import (pick a file; it is
  validated, then copied into `schemes/`). Name clashes on import ask whether to replace or
  keep both.

**The editor.**
- A window like Settings and the profile editor, opened from Settings, the profile editor and
  the palette.
- A list of schemes (built-ins marked read-only, user schemes editable) and, for the selected
  scheme: a color picker per entry, optional font family and size, and a **live preview**: a
  fake terminal showing a prompt, colored output in all 16 colors, a selection and the cursor.
- Edits preview immediately in the editor and apply to open sessions when saved.

**Pane accents.** A profile's accent already tints its tab. In a split tab, a pane whose
profile has an accent shows it on the pane border, so each host is recognizable even with the
same scheme.

Settled after the builder check *(my calls, 2026-10-27; the owner can change any of them)*:

- **A scheme font resizes its sessions.** A pane whose scheme sets a font gets that font's cell
  size, so its rows and columns (and the size reported to the program and the SSH channel)
  follow, through the existing per-pane resize path. Panes with different fonts may share a
  split tab. Saving such a scheme reflows the affected open sessions once.
- **References by stable id.** Each scheme file carries a stable `id` and a display `name`;
  profiles and settings refer to the id, so renames never break them. Built-in schemes have
  fixed ids. Deleting a scheme in use is allowed after a confirmation that names how many
  profiles use it; those profiles fall back to the global scheme. A profile or setting that
  refers to a missing scheme uses the global scheme (or Graphite, for a missing global) and
  says so once in the settings notice.
- **Default look unchanged.** The default global scheme stays Graphite (dark) whatever the app
  theme, as today. A light Graphite companion ships as a built-in to choose explicitly; there
  is no automatic "follow the app theme" option in this version.
- **Colors stored flat.** Every scheme entry, selection included, is a plain `"#rrggbb"`
  color; the editor previews them exactly as stored. The 256-color cube and grayscale ramp stay
  computed as today and are not part of a scheme.
- **Contrast.** Built-ins pass the existing contrast tests (`palette.rs`), extended to every
  built-in. In the editor, low contrast between text and background shows a non-blocking hint;
  saving is always allowed.
- **Pane accents only in split tabs.** A pane's accent border shows only when its tab has more
  than one pane; a pane whose profile has no accent shows no accent border; the focused pane
  keeps its focus outline (combined with the accent as the builder decides). A tab back to one
  pane looks as today.

## 5. Constraints

- Everything in the main design and `CLAUDE.md` holds: cross-platform code, Graphite look for
  the chrome, no naming of other terminal applications (built-in schemes need names of their
  own, and any palette taken from elsewhere needs a license compatible with Apache-2.0,
  recorded in `LICENSES.md`), secrets untouched, Conventional Commits with no attribution.
- Redraw contract: changing a scheme redraws affected visible panes once (and resizes them once when the font changes); nothing periodic.
- Scheme files are plain TOML, never contain secrets, and are validated on load (colors as
  `"#rrggbb"`; unknown keys reported in the settings notice like other config files).

## 6. Left to the builder

- Which built-ins ship besides Graphite (a light companion of Graphite is expected; a few more
  is fine), their names, and their contrast tests.
- How the pane accent border and the focus outline combine (e.g. focused pane outlined in its
  accent instead of violet); single-pane tabs stay as today.
- The exact scheme file schema, how a profile refers to a scheme (stable id vs file name), and
  how renames keep references working.
- Editor layout, the color picker widget, keyboard handling, and where the preview text comes
  from.
- Whether the global scheme follows the app's Dark/Light theme by default (today the terminal
  stays Graphite dark in both); any change must keep today's look for existing users.
