---
id: color-schemes
title: Color schemes
sidebar_label: Color schemes
---

A color scheme sets the colors of a terminal session: the text and background, the cursor, the selection and the 16 ANSI colors that programs use. A scheme can also name a font family and size. The global scheme colors the whole app, the tab bar, dialogs, Settings and the palette included; a profile's own scheme colors only that profile's sessions.

![The Color schemes tab with a custom scheme selected: a live preview, the colors and the buttons below](/img/color-schemes.png)

## Global scheme and profile override

Every session uses the global scheme unless its profile picks another one.

- **Global.** In Settings, the **Color scheme** row chooses the scheme for every session and for the app around them: the tab bar, Settings, the palette, dialogs and editors take its background, text and accent. It is the `color_scheme` key of [`settings.toml`](files#settingstoml). The default is Graphite, a dark scheme, so nothing changes until you pick another. For a light window, choose Graphite Light. The old `theme` setting is gone: while `settings.toml` still has it, fbterm reports it each time it reads the file, until fbterm next writes the file (for example when you change something in Settings), which drops the key. You can also delete the line yourself.
- **Per profile.** In the profile editor, the **Color scheme** field starts at **Use global scheme**. Choosing a scheme there sets the `color_scheme` field of the [profile](profiles#fields-common-to-every-profile). Use it to make a production host look different from the rest. A profile's scheme colors only its sessions; the rest of the app keeps the global scheme.

In a split tab each pane uses the scheme of its own profile. A tab or pane that is already open changes at once when you change a setting, a profile or a scheme.

## Built-in schemes

| Name | Id | Look | Suggested font |
| --- | --- | --- | --- |
| Graphite | `graphite` | The default: the dark Graphite palette with a violet cursor. | Geist Mono (fbterm's default font) |
| Graphite Light | `graphite-light` | A light companion on a near-white background. | Geist Mono |
| Harbor | `harbor` | Deep teal-black with an aqua cursor. | JetBrains Mono |
| Ember | `ember` | Coal-brown with a warm orange cursor. | IBM Plex Mono |

Built-in schemes cannot be changed or deleted; duplicate one to start your own. Each meets a readability bar: text against the background at a contrast ratio of at least 7, the ANSI colors 1 to 6 and 9 to 14 against the background at least 4.5, and the selection against the text at least 4.5.

## The Color schemes tab

Color schemes is a tab in a terminal window's tab bar, like Settings and Profiles: there is one in the app, and opening it again selects it. Open it with **Edit schemes…** next to the **Color scheme** row in Settings (or in the profile editor), or with **Color schemes** in the palette. It is closed like any tab (`Cmd+W`) and is not restored at relaunch. The list on the left holds the built-in schemes and yours; the right side shows the selected scheme with a live preview of a prompt, output in every color, a selection and the cursor.

- **Edit colors.** Click a swatch to show its R, G and B sliders, or type a `#rrggbb` value. The preview follows every change.
- **Font.** Under **Font**, choose a **Family** (or **Use global font**) and a **Size**.
- **Duplicate** copies the selected scheme under a new name. **Rename** changes the name; the id stays, so profiles that use the scheme keep working.
- **Delete** removes a scheme of yours. It asks `Delete "name"?` and, when profiles use it, says how many: "2 profiles use it and will switch to the global scheme." Choose **Delete** or **Cancel**.
- **Import…** reads a scheme file. If a scheme with the same id is already installed, a question offers **Replace**, **Keep both** (the default, on `Enter`) and **Cancel**. Keeping both installs the import under a new id and adds "(imported)" to its name.
- **Export…** writes the selected scheme to a file you choose, to share it.
- **Save** writes your changes to disk and applies them to open sessions. Until then a scheme row shows **Unsaved changes**.

For your own schemes, if the text against the background, any of the ANSI colors 1 to 6 and 9 to 14 against the background, or the selection against the text falls below the readability bar above, the editor shows "Text may be hard to read on this background." The hint is only a warning and never stops you from saving.

## The theme gallery

Next to the editor, the Color schemes tab has an **Editor | Gallery** switch. The gallery lists ready-made schemes made by others, so you can try a look without building it by hand.

- **Browsing.** Rows are sorted by name. The search box matches the name, the project and the author, and **All**, **Dark** and **Light** narrow the list. Each row shows a preview, its credit (project, author and license) and a **Project page** link.
- **Install.** **Install** saves the theme as a scheme of yours, a file named `gallery-<id>.toml` in the `schemes/` folder, and the button then reads **Installed**. It appears in Settings and the profile editor like any other scheme, and you can duplicate or edit it.
- **Update.** When the catalog has a newer version, the button reads **Update** and replaces the installed file in place: profiles keep using it and open panes recolor. If you changed the theme on this machine (its colors, name or font), or it was imported from a file or a profile archive rather than installed here, fbterm asks first: **Replace my changes**, **Install as a copy** (the default, on `Enter`) or **Cancel**. The copy is a scheme of yours that keeps the credit. A theme that the catalog no longer offers and that you have installed shows the note **No longer offered** instead of a button; the installed copy keeps working.
- **Credits.** An installed theme keeps its credit in its file (the `[credit]` table), shows it in the editor and keeps it when you duplicate or export the scheme. Exports keep only the credit, not the record of what was installed, so the receiving copy counts as changed. A credit with hidden or direction-changing characters is dropped when a file or archive is read; the colors are kept. This works offline.

### What the gallery contacts

The gallery reads one file, `https://raw.githubusercontent.com/featherbitplatform/terminal-themes/main/catalog.toml`, and only when you open the gallery while no catalog has loaded yet in this session (the first time, and again after an error, when you return to the Gallery tab or reopen the Color schemes tab), or when you press **Refresh** or **Retry**. Once a catalog has loaded, it is shown again from memory until you quit. Nothing is fetched at startup or in the background, no check repeats, and nothing is cached on disk. Programs running in your sessions can never trigger it. If you are offline, the gallery shows one error line with **Retry**; your installed themes keep working.

The themes come from [terminal-themes](https://github.com/featherbitplatform/terminal-themes), each under a permissive license with credit to its author. The full credits, and how to propose a new theme by pull request, are in that repository.

## Fonts in schemes

A scheme that names a font gives its sessions that family and size instead of the global font in Settings. The size of a character cell follows the font, so the rows and columns of those panes change with it, and the program (or the SSH channel) is told the new size once. Panes with different fonts can sit in the same split tab. Saving a scheme with a different font reflows its open sessions once.

Whatever the font, block elements and box-drawing lines are drawn by fbterm itself to fill their cells, so frames and block art look the same in every font and line up between panes.

## Suggested fonts

A scheme can suggest a font that suits it, with a link to the font's official download page. The built-in schemes and every theme in the gallery carry one. A suggestion changes nothing by itself: fbterm never ships, downloads or installs fonts.

In the Color schemes tab, the selected scheme (and each gallery row) shows **Suggested font** with the family and:

- **Download** opens the font's page in your browser; the button shows the site it leads to, such as `Download (github.com)` (a long host name shows its end, the part that names the site's owner). Download and install the font there, the way your system installs fonts.
- **Use font** makes it the global **Font family** in Settings. It is available once the font is installed; the suggestion itself is not written into the scheme, so an installed gallery theme keeps receiving updates.
- **In use** when the scheme's sessions already use that font, or **Not installed** when your system does not have it. A scheme that sets its own font (under **Font**) keeps it for its sessions, and the line says so.

The check looks at the fonts fbterm can draw with. A suggested font you install while fbterm runs is picked up the next time you show the Color schemes tab or its gallery, or focus its window (or turn on the checkbox below): fbterm looks through the system's fonts again for the suggested families it is missing and adds just those, without a restart.

### Use each theme's suggested font

With **Use each theme's suggested font** checked (in the Color schemes tab, or the row of the same name in Settings, both the `use_theme_fonts` setting in [`settings.toml`](files#settingstoml)), choosing another scheme, globally or for a profile, also switches its sessions to that scheme's suggested font. The font a session uses is then:

1. the scheme's own font, if it sets one;
2. else the scheme's suggested font, if it is installed;
3. else the global **Font family**.

A missing font falls back to the global one without a message (the scheme's line shows **Not installed**). Nothing is written into schemes or into the font setting, so unchecking the box restores the global font everywhere. Sessions whose font changes reflow once, as with a scheme font.

## Files and sharing

Each scheme of yours is one file in the `schemes/` folder of the [configuration directory](files#configuration-directory), named after its id: `<id>.toml`. Built-in schemes are part of the app and are not files. Themes installed from the [gallery](#the-theme-gallery) are files named `gallery-<id>.toml`. Edits to these files while the app runs are picked up at once.

Ids of your schemes look like `user-` followed by a random UUID; the built-in ids are `graphite`, `graphite-light`, `harbor` and `ember`. Scheme files are plain text, hold no secrets and are limited to 64 KiB. The file name must match the id inside. To share a scheme, use **Export…** and send the file; the other person uses **Import…**.

A file that cannot be read, or a profile or setting that names a scheme that does not exist, never stops a session: the session uses the global scheme (Graphite when the global one is missing too), and the banner at the top of the window says so once.

## File format

```toml
id = "user-5b0e6c1a-3f4d-4c8e-9a52-7d1f0b2c9e44"
name = "Slate Dusk"
font_family = "Geist Mono"   # optional
font_size = 14.0             # optional

[colors]
foreground = "#e6e9f0"
background = "#14161c"
cursor = "#f2b96b"
selection = "#2f3a52"
black = "#242833"
red = "#f0707a"
green = "#7fd6a0"
yellow = "#e9c46a"
blue = "#76a9f0"
magenta = "#c39bf0"
cyan = "#6fd3dc"
white = "#c5cad6"
bright_black = "#6b7285"
bright_red = "#ff949c"
bright_green = "#a3e6bc"
bright_yellow = "#f5d98f"
bright_blue = "#9fc3f7"
bright_magenta = "#d8b9f7"
bright_cyan = "#97e3e9"
bright_white = "#f6f8fc"

[suggested_font]             # optional
family = "JetBrains Mono"
url = "https://www.jetbrains.com/lp/mono/"
license = "OFL-1.1"
```

| Key | Meaning |
| --- | --- |
| `id` | Stable id that profiles and `settings.toml` refer to. Required. |
| `name` | Name shown in the list and in Settings. Required. |
| `font_family`, `font_size` | Optional font for the scheme's sessions. `font_size` must be a positive number. |
| `[colors]` | Every key shown above, each as `"#rrggbb"`. `selection` is the selection background as it should look on the background (a solid color, not a transparency). |
| `[suggested_font]` | Optional [suggestion](#suggested-fonts): `family` (the name exactly as your system lists the installed font), `url` (the font's official page: a plain `https://` link whose host is a name, with no user part, `%` escapes or backslashes) and `license` (one of `OFL-1.1`, `Apache-2.0`, `MIT`, `BSD-2-Clause`, `BSD-3-Clause`, `ISC`, `Unlicense`, `CC0-1.0`, `Zlib`), all three required, each at most 2000 characters. A table that breaks these rules, or holds hidden or direction-changing characters, is ignored with a line in the banner; the scheme still loads. It travels with the scheme like the credit (duplicate, export, archives) and never counts as a change to a gallery theme. |

The 256-color cube and the grayscale ramp are computed and are not part of a scheme.

## Pane accent borders

In a [split tab](tiling), a pane whose profile has an accent color gets a thin border in that accent, so hosts stay recognizable side by side. A pane without an accent has no border. The focused pane keeps its outline, drawn in its accent when it has one. A tab with a single pane looks as before, with no border.
