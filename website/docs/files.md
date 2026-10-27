---
id: files
title: Where files live
sidebar_label: Where files live
---

featherbit terminal keeps its configuration in one directory per user.

## Configuration directory

| System | Directory |
| --- | --- |
| macOS | `~/Library/Application Support/io.featherbit.fbterm` |
| Linux | `~/.config/fbterm` (respects `XDG_CONFIG_HOME`) |
| Windows | `%APPDATA%\featherbit\fbterm\config` |

Uninstalling the app never removes this directory (see [Install on Linux](install-linux#uninstall) and [Install on Windows](install-windows#upgrade-and-uninstall)). The directory is created the first time the app writes a file. All files are TOML and are written atomically (a temporary file in the same directory, then a rename), so a crash never leaves a half-written file.

| File | Written by | Purpose |
| --- | --- | --- |
| `profiles.toml` | you (and the profile editor) | Saved local and SSH profiles, with their tunnels. See [Profiles and jump hosts](profiles) and [Tunnels](tunnels). |
| `settings.toml` | you (and the Settings window) | Font, scrollback, theme, background, keyboard protocol and changed keyboard shortcuts. |
| `schemes/` | you (and the Color schemes window) | One `<id>.toml` file per color scheme of yours. See [Color schemes](color-schemes#files-and-sharing). |
| `secrets.fbvault` | the app | Your saved passwords and passphrases, encrypted with your main password. Only exists once you use the encrypted vault. See [Saved secrets](saved-secrets). |
| `secrets.dpapi` | the app | Windows only: your saved passwords and passphrases, encrypted for your Windows account, when saved secrets use the system storage. See [Saved secrets](saved-secrets). |
| `state.toml` | the app | Open windows and tabs, and when each profile was last used. |

`profiles.toml` and `settings.toml` are reloaded while the app runs. If a file has a syntax error, the app reports the file and line, keeps what it had loaded, and never overwrites the broken file.

## settings.toml

Every field is optional. A missing file, or a missing field, takes the default shown here.

The easiest way to change a setting is the Settings window (`Cmd+,` on macOS, `Ctrl+,` on Linux and Windows, or **Settings** in the palette). Every change applies at once and is written to `settings.toml` in place: only that setting's line changes, and your comments, blank lines, the order of the settings and everything else stay as they are. A setting that is only shown as a comment (as in the file the app creates) is set on the line right after its comment. If `settings.toml` is a symlink (for example into a dotfiles folder), the file it points to is edited and the link stays; the file keeps its permissions. While `settings.toml` has an error the window is read-only and the file is never written; fix it by hand and the window follows. Edits made to the file while the window is open show in the window at once.

To edit the file by hand, run **Open settings file** from the palette (`Cmd+Shift+P` or `Ctrl+Shift+P`). If `settings.toml` does not exist, the app creates it with every setting written out as a comment, showing its default and what it does, and opens it in your default app. Remove the leading `#` of a line to change that setting. An existing file is never overwritten. **Reveal config folder** shows the folder in your file manager.

```toml
font_family = "Geist Mono"
font_size = 13.0
scrollback_lines = 10000
theme = "dark"
color_scheme = "graphite"
kitty_keyboard = false
background_opacity = 1.0
background_blur = false
window_decorations = "fbterm"
max_fps = 60
tunnel_max_retries = 10
secret_storage = "auto"
```

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `font_family` | string | `"Geist Mono"` | Terminal font family. |
| `font_size` | number, `6.0` to `72.0` | `13.0` | Font size in points. |
| `scrollback_lines` | integer, `0` to `1000000` | `10000` | Lines of history kept per tab, open tabs included. A change applies at once. Raising it keeps everything; lowering it drops the oldest lines of open tabs that hold more, and in the Settings window that asks first (Apply or Cancel), because it cannot be undone. Lowering it in the file applies when you save and a banner says how many tabs lost lines. Above `100000`, resizing a window can pause briefly while the history rewraps. |
| `theme` | `"dark"` or `"light"` | `"dark"` | Color theme. |
| `color_scheme` | string (scheme id) | `"graphite"` | Color scheme for every session; a profile can override it. An id that matches no scheme uses Graphite and is reported in the banner. See [Color schemes](color-schemes). |
| `kitty_keyboard` | `true` or `false` | `false` | Let programs turn on the kitty keyboard protocol, which reports keys such as Esc, Ctrl and Alt chords, and optionally every key and key releases, as unambiguous escape codes. When `false`, programs' requests for it are ignored and keys use the classic encoding. A change applies to open tabs at once: turning it off returns every tab to the classic encoding, turning it on lets programs enable the protocol from then on (restart a program that asked for it earlier). |
| `background_opacity` | number, `0.05` to `1.0` | `1.0` | Opacity of the terminal's background and of the tab bar, from `0.05` (nearly see-through) to `1.0` (opaque). Text, the cursor, the selection and cells a program gives their own background color stay opaque. On macOS a change applies to open windows at once; on Linux and Windows a window can only become see-through if it was opened while the value was below `1.0`, so open a new window after lowering it (or restart fbterm: while a window opened at `1.0` is open, Settings shows `Restart fbterm to apply this to open windows.` under the control). |
| `background_blur` | `true` or `false` | `false` | Blur what shows through a see-through background. Takes effect only for windows opened while `background_opacity` is below `1.0`, so open a new window after changing it. Works on macOS, on Windows 11 22H2 and newer, and on KDE Plasma under Wayland; elsewhere the Settings switch is disabled and reads `Not supported on this system`. |
| `window_decorations` | `"fbterm"` or `"system"` | `"fbterm"` | Linux only; other systems ignore it. Who draws the window's title bar: `"fbterm"` draws it in the tab bar, `"system"` uses the desktop's. In the file it is written as `# Window title bar on Linux: "fbterm" draws it in the tab bar, "system" uses the desktop's.` In Settings this is **Title bar**. If the desktop refuses to let fbterm move its window, the session uses the system title bar anyway and says so once; the setting stays as you wrote it. See [Install on Linux](install-linux#title-bar). |
| `max_fps` | integer, `30` to `240`, or `"display"` | `60` | Most redraws per second caused by program output. Typing, scrolling, selecting and resizing always redraw at once. `"display"` follows the refresh rate of the screen your windows are on (the fastest one, if they are on several screens); where the rate cannot be detected (Linux and Windows for now) it means `60`. Higher values use more CPU while output streams. Applies at once. |
| `tunnel_max_retries` | integer, `0` to `100` | `10` | How many times a [tunnel](tunnels#reconnecting) tries to reconnect on its own after its connection drops. `0` turns automatic reconnecting off. In the Settings window this is **Reconnect attempts** under Tunnels. Applies at once. |
| `secret_storage` | `"auto"`, `"keychain"` or `"vault"` | `"auto"` | Where [saved secrets](saved-secrets) are kept: the system storage where there is one (else the encrypted vault), the system storage, or the vault. In the Settings window this is **Saved secrets**, which also moves the secrets when you change it. A change made by hand while the app runs takes effect at the next launch and moves nothing, so prefer Settings. |

### Redraw rate

When a program prints a lot (a log stream, a build), the terminal does not redraw for every piece of output: it draws at most `max_fps` times per second and shows everything that arrived in between. What you type is different: a keypress, and the echo that comes back from it, are drawn right away whatever the setting. The default of `60` is smooth on most screens and keeps CPU use low. On a 120 Hz screen, `max_fps = 120` or `max_fps = "display"` makes fast-scrolling output smoother, at the cost of more CPU while output is streaming (no extra cost while nothing prints). In the Settings window this is **Redraw rate** under Terminal: 30, 60, 120 or **Match display**, plus your own number if `settings.toml` has another one.

A value outside its range does not reject the file: the app uses the nearest allowed value (for example `font_size = 100` gives `72`; a `max_fps` that is neither a whole number nor `"display"` gives `60`) and reports the field, the file and the line the same way it reports a syntax error. The file itself is left as you wrote it.

### Keyboard shortcuts

Shortcuts you change are kept in a `[keys]` table at the end of `settings.toml`; only the changes are stored, so a missing table means every shortcut has its default. The format and the action names are in [Keyboard shortcuts](keyboard-shortcuts#the-keys-table).

## state.toml

This file belongs to the app. It records which windows and tabs were open, each window's size, and the last time each profile was opened, so the layout can be restored and the palette can rank recent profiles first. It is safe to delete; the app then opens a default window. Do not edit it by hand.

It also keeps the [file panel](file-panel#in-statetoml)'s width (`file_panel_width`) and, once you turn hidden files off, `file_panel_show_hidden = false`.

## Saved secrets

Passwords and key passphrases are never written to `profiles.toml`, `settings.toml` or `state.toml`. They are kept in one of three places, chosen by the `secret_storage` setting: the system keychain (one item, service `fbterm`, account `secrets`) on macOS and Linux, a DPAPI-encrypted file `secrets.dpapi` in this directory on Windows, or the encrypted vault `secrets.fbvault` in this directory. If you delete a profile its secrets are removed. See [Saved secrets](saved-secrets).

## Host keys

SSH host keys are checked against your OpenSSH file at `~/.ssh/known_hosts`. See [Profiles and jump hosts](profiles#host-keys).
