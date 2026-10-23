---
id: keyboard-shortcuts
title: Keyboard shortcuts
sidebar_label: Keyboard shortcuts
---

Application shortcuts use `Cmd` on macOS. On Linux and Windows every shortcut is `Ctrl+Shift`, so plain `Ctrl` keys (such as `Ctrl+T`, `Ctrl+W` and `Ctrl+P`) keep reaching your shell and the programs running in it.

On macOS the shortcuts never use `Ctrl` or `Option`. Copy and paste are handled by the terminal view and are not in this table. Programs running in the terminal, including on SSH hosts, cannot read or change your clipboard.

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

### The title bar on macOS

On macOS the tab bar is the title bar: the window buttons sit at its left end and the tabs follow them. Drag any empty part of the tab bar (around and after the tabs) to move the window, and double-click it to zoom the window or restore its size. The profile editor window works the same way with its own header. The window title, shown in the Dock, Mission Control and the Window menu, is the active tab's name.

On Linux and Windows the window keeps the system title bar for now.

## Scrollback

| Action | macOS | Linux and Windows |
| --- | --- | --- |
| Scroll up one page | `Shift+PageUp` | `Shift+PageUp` |
| Scroll down one page | `Shift+PageDown` | `Shift+PageDown` |
| Jump to the top of the history | `Shift+Home` or `Cmd+Home` | `Shift+Home` |
| Jump back to the bottom | `Shift+End` or `Cmd+End` | `Shift+End` |

A page is the height of the terminal less one line, so the last line of the previous page stays in view. Typing anything returns to the bottom. While a full-screen program (an editor, a pager, a system monitor) uses the alternate screen there is no scrollback, so these keys go to the program instead.

## Palette and settings

| Action | macOS | Linux and Windows |
| --- | --- | --- |
| Open the palette | `Cmd+Shift+P` | `Ctrl+Shift+P` |
| Open the profile editor | `Cmd+,` or `Cmd+Shift+,` | `Ctrl+Shift+,` |

The profiles button after the new-tab button in the tab bar opens the profile editor too. Settings themselves are edited in [`settings.toml`](files#settingstoml).

## Palette actions

These actions are listed after your profiles. Type part of a name to find one.

| Action | Does |
| --- | --- |
| New tab | Choose a profile to open in a new tab. |
| New window | Open a new window. |
| Close tab | Close the active tab. |
| Edit profiles | Open the profile editor. |
| Open settings file | Open [`settings.toml`](files#settingstoml) in your default app. If the file does not exist yet, it is first created with every setting written out as a comment, showing its default. |
| Reveal config folder | Show the config folder in your file manager, with `settings.toml` selected on macOS when it exists. The folder is created if it is missing. |
| Quit | Save the layout and quit. |

## Inside the palette

The palette searches profiles and actions as you type. These keys work while it is open:

| Key | Does |
| --- | --- |
| `Up` or `Ctrl+P` | Move the selection up |
| `Down` or `Ctrl+N` | Move the selection down |
| `Enter` | Run the selected profile or action |
| `Escape` | Close the palette |

You can also click a row. The palette shows ten rows at a time.
