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

The directory is created the first time the app writes a file. All files are TOML and are written atomically (a temporary file in the same directory, then a rename), so a crash never leaves a half-written file.

| File | Written by | Purpose |
| --- | --- | --- |
| `profiles.toml` | you (and the profile editor) | Saved local and SSH profiles. See [Profiles and jump hosts](profiles). |
| `settings.toml` | you | Font, scrollback, theme and keyboard protocol. |
| `state.toml` | the app | Open windows and tabs, and when each profile was last used. |

`profiles.toml` and `settings.toml` are reloaded while the app runs. If a file has a syntax error, the app reports the file and line, keeps what it had loaded, and never overwrites the broken file.

## settings.toml

Every field is optional. A missing file, or a missing field, takes the default shown here.

```toml
font_family = "Geist Mono"
font_size = 13.0
scrollback_lines = 10000
theme = "dark"
kitty_keyboard = false
```

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `font_family` | string | `"Geist Mono"` | Terminal font family. |
| `font_size` | number, `6.0` to `72.0` | `13.0` | Font size in points. |
| `scrollback_lines` | integer, `0` to `1000000` | `10000` | Lines of history kept per tab. |
| `theme` | `"dark"` or `"light"` | `"dark"` | Color theme. |
| `kitty_keyboard` | `true` or `false` | `false` | Let programs turn on the kitty keyboard protocol, which reports keys such as Esc, Ctrl and Alt chords, and optionally every key and key releases, as unambiguous escape codes. When `false`, programs' requests for it are ignored and keys use the classic encoding. A change applies to open tabs at once: turning it off returns every tab to the classic encoding, turning it on lets programs enable the protocol from then on (restart a program that asked for it earlier). |

A value outside its range does not reject the file: the app uses the nearest allowed value (for example `font_size = 100` gives `72`) and reports the field, the file and the line the same way it reports a syntax error. The file itself is left as you wrote it.

## state.toml

This file belongs to the app. It records which windows and tabs were open, each window's size, and the last time each profile was opened, so the layout can be restored and the palette can rank recent profiles first. It is safe to delete; the app then opens a default window. Do not edit it by hand.

## Secrets and the keychain

Passwords and key passphrases are never written to any of these files. They are stored in the system keychain as a single item:

| Field | Value |
| --- | --- |
| Service | `fbterm` |
| Account | `secrets` |

The item holds all saved secrets, keyed by profile. If you delete a profile its secrets are removed. If the keychain is unavailable, the app keeps typed passwords for the current session only and says so in the profile editor.

## Host keys

SSH host keys are checked against your OpenSSH file at `~/.ssh/known_hosts`. See [Profiles and jump hosts](profiles#host-keys).
