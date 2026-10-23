---
id: keyboard-shortcuts
title: Keyboard shortcuts
sidebar_label: Keyboard shortcuts
---

Application shortcuts use `Cmd` on macOS. On Linux and Windows every shortcut is `Ctrl+Shift`, so plain `Ctrl` keys (such as `Ctrl+T`, `Ctrl+W` and `Ctrl+P`) keep reaching your shell and the programs running in it.

On macOS the shortcuts never use `Ctrl` or `Option`. Copy and paste are handled by the terminal view and are not in this table.

## Windows and tabs

| Action | macOS | Linux and Windows |
| --- | --- | --- |
| New tab | `Cmd+T` | `Ctrl+Shift+T` |
| Close tab | `Cmd+W` | `Ctrl+Shift+W` |
| Next tab | `Cmd+Shift+]` | `Ctrl+Shift+]` |
| Previous tab | `Cmd+Shift+[` | `Ctrl+Shift+[` |
| Select tab 1 to 9 | `Cmd+1` to `Cmd+9` | `Ctrl+Shift+1` to `Ctrl+Shift+9` |
| New window | `Cmd+N` | `Ctrl+Shift+N` |
| Quit (saves the layout first) | `Cmd+Q` | `Ctrl+Shift+Q` |

## Palette and settings

| Action | macOS | Linux and Windows |
| --- | --- | --- |
| Open the palette | `Cmd+Shift+P` | `Ctrl+Shift+P` |
| Open the profile editor | `Cmd+,` or `Cmd+Shift+,` | `Ctrl+Shift+,` |

The settings shortcut currently opens the profile editor. Settings themselves are edited in [`settings.toml`](files#settingstoml).

## Inside the palette

The palette searches profiles and actions as you type. These keys work while it is open:

| Key | Does |
| --- | --- |
| `Up` or `Ctrl+P` | Move the selection up |
| `Down` or `Ctrl+N` | Move the selection down |
| `Enter` | Run the selected profile or action |
| `Escape` | Close the palette |

You can also click a row. The palette shows ten rows at a time.
