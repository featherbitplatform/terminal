---
id: keyboard-shortcuts
title: Keyboard shortcuts
sidebar_label: Keyboard shortcuts
---

Application shortcuts use `Cmd` on macOS. On Linux and Windows shortcuts are `Ctrl+Shift`, so plain `Ctrl` keys (such as `Ctrl+T`, `Ctrl+W` and `Ctrl+P`) keep reaching your shell and the programs running in it. The exceptions are `Ctrl+,` for Settings, `Ctrl+Alt+F` for find in all tabs and `Ctrl+Alt` with an arrow key to move between panes.

These are the defaults. Every application shortcut can be changed or removed in the **Keyboard** section of the Settings window (see [Changing shortcuts](#changing-shortcuts)); the tables below list the defaults, and the Settings window, the palette and the tab-bar tooltips always show the shortcuts in force.

Hover over a button in the tab bar to see what it does and its shortcut. The gear button at the end of the tab bar opens Settings. The tunnels button after the tab buttons opens or closes the [tunnels panel](tunnels#the-tunnels-panel), shows how many tunnels are up and is highlighted while the panel is open. While the focused pane is an SSH session whose server offers SFTP, a folder button appears before the gear: it opens or closes the [file panel](file-panel) and is highlighted while the panel is open.

On macOS the shortcuts use `Cmd`, and `Option` only for moving between panes; never `Ctrl`. Copy and paste are handled by the terminal view, see [Copy and paste](#copy-and-paste). Programs running in the terminal, including on SSH hosts, cannot read or change your clipboard.

## Copy and paste

Select text by dragging with the mouse (double-click selects a word, triple-click a line, `Shift`+click extends the selection).

| Action | macOS | Linux and Windows |
| --- | --- | --- |
| Copy the selection | `Cmd+C` | `Ctrl+Shift+C` |
| Paste | `Cmd+V` | `Ctrl+Shift+V` |

On Linux and Windows `Ctrl+C` is not copy: it goes to the program as usual (it interrupts the running command in a shell). A paste into a program that asked for bracketed paste is sent bracketed, so a shell does not run pasted lines on its own.

**Right-click** on macOS: it opens a small menu at the pointer with **Copy** and **Paste** and focuses the pane you clicked. **Copy** is enabled when something is selected and copies it exactly like `Cmd+C`, leaving it selected; **Paste** pastes into that pane exactly like `Cmd+V`.

**Right-click** on Linux and Windows: on a selection it copies it to the clipboard and clears the selection. With nothing selected it focuses the pane you clicked and opens the same menu with **Copy** disabled (nothing is selected) and **Paste**, which pastes exactly like `Ctrl+Shift+V`.

On every system another right-click while the menu is open does the same at the new point. `Up` and `Down` move in the menu, `Enter` chooses, and `Escape`, a click outside it, switching to another window or resizing the window close it. When the program in the pane uses the mouse itself (an editor or a system monitor with mouse support), a plain right-click goes to the program; hold `Shift` while right-clicking to copy or open the menu instead. Only your own right-click does any of this; nothing a program prints can open the menu or touch the clipboard.

**Dropping files** from Finder, Explorer or your file manager onto a terminal window types their paths into the focused pane, as a paste: several files go on one line, separated by spaces, with a space after the last and no Enter, so nothing runs until you press it. A path with spaces or shell characters is quoted for the pane's shell (single quotes on macOS, Linux and in SSH sessions, double quotes in a local shell on Windows). A pane that is connecting or whose session ended gets nothing. While the window's [file panel](file-panel#uploading) shows a server, a drop uploads the files there instead.

These shortcuts cannot be changed; Settings lists them under **Fixed** at the end of the **Keyboard** section.

## Windows and tabs

| Action | macOS | Linux and Windows |
| --- | --- | --- |
| New tab | `Cmd+T` | `Ctrl+Shift+T` |
| Close pane (the last pane closes the tab) | `Cmd+W` | `Ctrl+Shift+W` |
| Next tab | `Cmd+Shift+]` | `Ctrl+Shift+]` |
| Previous tab | `Cmd+Shift+[` | `Ctrl+Shift+[` |
| Select tab 1 to 9 | `Cmd+1` to `Cmd+9` | `Ctrl+Shift+1` to `Ctrl+Shift+9` |
| New window | `Cmd+N` | `Ctrl+Shift+N` |
| Quit (saves the layout first) | `Cmd+Q` | `Ctrl+Shift+Q` |
| Show or hide the [file panel](file-panel) | `Cmd+Shift+E` | `Ctrl+Shift+E` |

## Panes

A tab can be split into panes, each with its own session. Splitting opens the focused pane's profile in a new pane beside or below it and moves focus there. A split is refused, silently, when a pane would end up smaller than 20 columns by 5 rows. These shortcuts act on the focused pane.

| Action | macOS | Linux and Windows |
| --- | --- | --- |
| Split right | `Cmd+D` | `Ctrl+Shift+D` |
| Split down | `Cmd+Shift+D` | `Ctrl+Shift+Alt+D` |
| Close pane | `Cmd+W` | `Ctrl+Shift+W` |
| Focus the pane to the left, right, above, below | `Cmd+Option+Left`, `Right`, `Up`, `Down` | `Ctrl+Alt+Left`, `Right`, `Up`, `Down` |
| Zoom in | `Cmd+=` | `Ctrl+Shift+=` |
| Zoom out | `Cmd+-` | `Ctrl+Shift+-` |
| Reset zoom | `Cmd+0` | `Ctrl+Shift+0` |
| Maximize pane (press again to restore) | `Cmd+Shift+Enter` | `Ctrl+Shift+Enter` |

Closing a pane closes its session, without asking. Moving focus goes to the nearest pane in that direction; when two are equally near, the one you used more recently. Zoom changes the text size of the focused pane only, one point per press, and the plus key zooms in however you type it (`Cmd+=`, `Cmd+Shift+=` or a `+` key of your own layout; elsewhere `Ctrl+Shift+=` or `Ctrl+Shift` with a `+` key, while a bare `Ctrl+=` still goes to the program); reset returns it to the font size in Settings. A maximized pane fills the tab while the others keep running out of sight; moving focus or splitting shows them again. Maximize does nothing in a tab with one pane.

Move tile, Move tile to new tab, Merge tab into and Find in this tab have no shortcut by default; they are in the palette and can be bound in Settings or in the [`[keys]` table](#the-keys-table). The page [Split panes](tiling) covers splitting, rearranging, maximize and restore in full.

### The title bar on macOS

On macOS the tab bar is the title bar: the window buttons sit at its left end and the tabs follow them. Drag any empty part of the tab bar (around and after the tabs) to move the window, and double-click it to zoom the window or restore its size. The profile editor and Settings windows work the same way with their own header. The window title, shown in the Dock, Mission Control and the Window menu, is the active tab's name.

On Linux and Windows the window keeps the system title bar for now.

## Scrollback

| Action | macOS | Linux and Windows |
| --- | --- | --- |
| Scroll up one page | `Shift+PageUp` | `Shift+PageUp` |
| Scroll down one page | `Shift+PageDown` | `Shift+PageDown` |
| Jump to the top of the history | `Shift+Home` or `Cmd+Home` | `Shift+Home` |
| Jump back to the bottom | `Shift+End` or `Cmd+End` | `Shift+End` |

A page is the height of the terminal less one line, so the last line of the previous page stays in view. Typing anything returns to the bottom. While a full-screen program (an editor, a pager, a system monitor) uses the alternate screen there is no scrollback, so these keys go to the program instead.

## Search

| Action | macOS | Linux and Windows |
| --- | --- | --- |
| Find in pane | `Cmd+F` | `Ctrl+Shift+F` |
| Find in all tabs | `Cmd+Shift+F` | `Ctrl+Alt+F` |

While the find bar is open:

| Key | Does |
| --- | --- |
| `Enter`, `Cmd+G` (`Ctrl+G`) | Next match up the scrollback (older) |
| `Shift+Enter`, `Cmd+Shift+G` (`Ctrl+Shift+G`) | Next match down the scrollback (newer) |
| `Escape` | Close the bar and return to the bottom |

`Ctrl+G` and `Ctrl+Shift+G` only move between matches while the bar is open; otherwise they reach the shell as usual. In the find-in-all-tabs box, `Up` and `Down` (or `Ctrl+P` and `Ctrl+N`) move the selection, `Enter` jumps to the selected match and `Escape` closes the box. See [Search](#search-in-detail) below.

## Palette and settings

| Action | macOS | Linux and Windows |
| --- | --- | --- |
| Open the palette | `Cmd+Shift+P` | `Ctrl+Shift+P` |
| Open Settings | `Cmd+,` | `Ctrl+,` |
| Open the profile editor | `Cmd+Shift+,` | `Ctrl+Shift+,` |

The profiles button after the new-tab button in the tab bar opens the profile editor too, and the gear button after it opens Settings. The Settings window has a control for every option in [`settings.toml`](files#settingstoml); changes apply at once and are saved to the file. In the profile editor and Settings windows, `Cmd+W` (`Ctrl+Shift+W`) closes the window, and each window's shortcut opens the other.

## Palette actions

These actions are listed after your profiles, each with its shortcut when it has one. Type part of a name to find one.

| Action | Does |
| --- | --- |
| New tab | Choose a profile to open in a new tab. |
| New window | Open a new window. |
| Close pane | Close the focused pane; the last pane closes the tab. |
| Split right | Open the focused pane's profile in a new pane to its right. |
| Split down | Open the focused pane's profile in a new pane below it. |
| Maximize pane | Let the focused pane fill the tab, or restore the others. |
| Merge tab into… | Choose another tab, then where this tab's panes land in it. |
| Move tile… | Choose where the focused pane goes within its tab. |
| Move tile to new tab | Turn the focused pane into a tab of its own, at the end of the bar. |
| Find in pane | Open the focused pane's find bar. |
| Find in all tabs | Open the search over every tab. |
| Find in this tab | Open the same search restricted to the panes of the current tab. |
| Edit profiles | Open the profile editor. |
| Duplicate profile | Save a copy of the focused pane's profile (in the profile editor, of the selected one) and open it in the editor; see [Duplicating a profile](profiles#duplicating-a-profile). It has no shortcut by default. |
| Settings | Open the Settings window. |
| Tunnels | Show or hide the [tunnels panel](tunnels#the-tunnels-panel) beside the terminal. It has no shortcut by default. |
| File panel | Show or hide the [file panel](file-panel) beside the focused pane's terminal. |
| Tunnel: _profile_ / _name_ | One row per tunnel, with its state on the right. Choosing a row switches that tunnel on or off. |
| Color schemes | Open the Color schemes window to duplicate, rename, edit and delete color schemes. It also opens from **Edit schemes…** in Settings and in the profile editor. |
| Open settings file | Open [`settings.toml`](files#settingstoml) in your default app. If the file does not exist yet, it is first created with every setting written out as a comment, showing its default. |
| Reveal config folder | Show the config folder in your file manager, with `settings.toml` selected on macOS when it exists. The folder is created if it is missing. |
| Export tab to file… | Save the active tab's text to a file; see [Exporting a tab](#exporting-a-tab). |
| Export profiles… | Open the [export window](export-import#exporting). It has no shortcut by default. |
| Import profiles… | Open the [import window](export-import#importing). It has no shortcut by default. |
| Quit | Save the layout and quit. |

## Inside the palette

The palette searches profiles and actions as you type. These keys work while it is open:

| Key | Does |
| --- | --- |
| `Up` or `Ctrl+P` | Move the selection up |
| `Down` or `Ctrl+N` | Move the selection down |
| `Enter` | Run the selected profile or action |
| `Shift+Enter` | Open the selected profile in a split to the right of the focused pane (an action just runs) |
| `Escape` | Close the palette |

You can also click a row. The palette shows ten rows at a time.

## Changing shortcuts

Open Settings (`Cmd+,` on macOS, `Ctrl+,` on Linux and Windows, or the gear button in the tab bar) and scroll to **Keyboard**. It lists every action with its shortcut, grouped into Tabs, Panes, Windows, Search and App.

- **Change one:** click its shortcut, then press the new one. While you record, keys go to the recorder only: they never reach a terminal and never run a shortcut. `Escape` cancels and keeps the old shortcut.
- **Already in use:** if another action has that shortcut, the row names it and offers **Take over** (the other action is then left without a shortcut) or **Cancel**. Two actions never share a shortcut.
- **Not allowed:** a shortcut needs `Cmd`, `Ctrl` or `Alt` (`Option`), except `F1` to `F12`, which work alone. Plain letters, `Enter`, `Tab` and `Escape` cannot be shortcuts on their own, and `Escape` never can.
- **Reserved:** keys that already do something outside this list cannot be taken: copy and paste in the terminal (`Cmd+C` and `Cmd+V` on macOS, `Ctrl+Shift+C` and `Ctrl+Shift+V` elsewhere), the scrollback keys (`Shift+PageUp`, `Shift+PageDown`, `Shift+Home`, `Shift+End`, and `Cmd+Home`, `Cmd+End` on macOS), Save in the profile editor (exactly `Cmd+S`, `Ctrl+S` elsewhere; with `Shift` or `Alt` added it is free to use) and, on macOS, `Cmd+Q` (Quit) and the app menu's `Cmd+H` (Hide) and `Cmd+Option+H` (Hide Others). The row says what uses the key.
- **Fixed shortcuts:** the end of the list, under **Fixed**, shows the shortcuts that cannot be changed, each marked `Fixed`: copy, paste, the scrollback keys, Save, and on macOS Hide and Hide others. Their shortcuts are not buttons and cannot be recorded.
- **Quit on macOS** is always `Cmd+Q`, so its row is fixed. While file transfers are running, the key goes through the app's own Quit, which first asks whether to cancel them; this needs a window in front, so with every window minimised `Cmd+Q` does nothing until one is shown again. With no transfers running, `Cmd+Q` quits at once from the app menu, as in any app. Choosing **Quit** in the app menu with the mouse always quits at once.
- **Taken from programs:** a shortcut that programs in the terminal would normally receive works, but its row shows a warning saying what is lost when that is known (for example ``Ctrl+R is reverse search in bash and zsh``). On macOS that is any shortcut without `Cmd`. On Linux and Windows it is `Ctrl` with a letter (without `Shift`), `Ctrl` with a few symbols, arrows and editing keys, any `Alt` shortcut (Meta in bash, zsh and emacs) and `F` keys on their own.
- **Remove one:** the `x` that appears when you point at a row leaves the action without a shortcut (it stays in the palette).
- **Go back:** a changed row has a reset button that restores its default, and **Restore defaults** restores them all.

Changes apply at once in every window, the palette and the tooltips, and are saved to `settings.toml`. The keys inside the palette, the find bar and the find-in-all-tabs box (`Enter`, `Shift+Enter`, `Escape`, `Up`, `Down`, `Ctrl+P`, `Ctrl+N`) stay as they are; the find bar's next and previous match shortcuts (`Cmd+G`, `Cmd+Shift+G`) can be changed like the others and still act only while the bar is open. `Cmd+S` in the profile editor, copy and paste and the scrollback keys are listed under **Fixed** and cannot be changed.

### The `[keys]` table

Only the shortcuts you change are saved, in a `[keys]` table in [`settings.toml`](files#settingstoml), one line per action:

```toml
[keys]
new_tab = "ctrl+shift+y"
close_tab = "none"
next_tab = "ctrl+tab"
open_settings_file = "cmd+shift+o"
```

A shortcut is its modifiers and a key joined by `+`. Modifiers are `ctrl`, `alt` (also `option`), `shift` and `cmd` (written `super` on Linux and Windows, where it is the Windows or Super key); the app writes them in that order, but any order and case are read. The key is a character (`t`, `1`, `,`, `]`), `f1` to `f12`, `up`, `down`, `left`, `right`, `pageup`, `pagedown`, `home`, `end`, `insert`, `delete`, `backspace`, `space`, `tab`, `enter` or `plus` for the `+` key. `"none"` leaves the action without a shortcut. `shift+}`, `shift+{` and `shift+<` are the same as `shift+]`, `shift+[` and `shift+,`; any other symbol is its own key, as your keyboard layout types it without `Shift` (on AZERTY, `ctrl+shift+&` is the `1` key). `f13` to `f24` work too, with a modifier.

| Name | Action |
| --- | --- |
| `new_tab`, `close_tab`, `next_tab`, `previous_tab` | New tab, close pane (the tab when it is the last), next and previous tab |
| `export_tab` | Export tab to file (no default shortcut) |
| `export_profiles`, `import_profiles` | Export and import profiles (no default shortcuts) |
| `duplicate_profile` | Duplicate profile (no default shortcut) |
| `split_right`, `split_down`, `maximize_pane` | Split the focused pane, maximize it |
| `focus_left`, `focus_right`, `focus_up`, `focus_down` | Move focus to the neighbouring pane |
| `zoom_in`, `zoom_out`, `zoom_reset` | Text size of the focused pane |
| `find_in_tab`, `merge_tab`, `move_tile`, `tile_to_tab` | Find in this tab (all panes of the tab), merge this tab into another, move the focused tile, move it to a tab of its own (no default shortcut) |
| `select_tab_1` to `select_tab_9` | Select tab 1 to 9 |
| `new_window`, `edit_profiles`, `settings`, `edit_schemes`, `tunnels`, `file_panel` | New window, the profile editor, Settings, the Color schemes window, the tunnels panel (no default shortcut), the file panel (`Cmd+Shift+E`, `Ctrl+Shift+E`) |
| `find`, `find_all`, `find_next`, `find_previous` | Find in pane, find in all tabs, older and newer match in the find bar |
| `palette`, `open_settings_file`, `reveal_config_folder`, `quit` | The palette, the palette actions of the same names, quit |

Edits to the table apply while the app runs, like every setting. An entry that names no action, a shortcut that cannot be read, is not allowed or is reserved, a shortcut another action already has, and `quit` on macOS are reported like other problems in `settings.toml`, with the file and line, and that action keeps its default; the rest of the file still applies. The Settings window writes only the lines of the actions you change there, keeping your comments (a comment above a removed line moves to the next one). An entry it could not read stays as you wrote it; an entry it could read but did not use (for example one that took another action's shortcut) is replaced when you change shortcuts there, so what runs is always what the file says.

## Search in detail

**Find in pane** opens a slim bar over the top right of the focused pane. Typing searches the pane's whole scrollback and screen as you type: every match in view is tinted, the current match is drawn in the accent color, and the view scrolls so the current match is visible. The counter reads `3 of 41` (`1 of 1000+` when there are more than a thousand; moving past the thousandth loads the older ones), `No matches` or `Invalid pattern`. Matches are numbered from the newest, so `Enter` walks back through the history and wraps around to the newest again. `Shift+Enter` from the newest match wraps to the oldest match loaded so far: with more than a thousand matches that is not the oldest in the history until you have moved past them with `Enter`. Up to 10,000 matches are kept loaded; past that, `Enter` wraps to the newest.

- **Case:** the search ignores case unless the query has an uppercase letter.
- **Plain text or regular expression:** plain text by default (every character matches itself). The `.*` button switches to regular expressions; an invalid one shows `Invalid pattern`. Queries longer than 512 characters show `Query too long`.
- **Wrapped lines** are searched as one line, so a match can continue on the next row. A match never spans a real line break.
- While the bar is open, keys go to the bar and never to the program in the tab. The tab keeps running; new output is searched too, and the count follows it.

**Find in all tabs** opens a box in the middle of the window, like the palette. It searches every tab of every window, including tabs that are not shown and tabs that are disconnected (they keep their history). Results appear tab by tab as each tab's search finishes, grouped under the tab's name and accent (and its window, when several are open), one row per match with the line around it. Each tab lists up to 50 matches and the box up to 500 in all, with a note when there are more. `Enter` (or a click) on a match brings its window forward, selects the tab, closes the box and opens that tab's find bar on the same query with that match current. In a tab with several panes each pane is its own group, labelled with the pane's title, and `Enter` also focuses that pane. **Find in this tab** from the palette opens the same box limited to the current tab's panes. In the palette, `Shift+Enter` on a profile opens it in a split to the right of the focused pane; clicking a row or plain `Enter` opens a new tab.

## Exporting a tab

**Export tab to file…** in the palette saves the active tab's text: its whole history, scrollback and screen, oldest line first. It has no shortcut by default; give it one in Settings, **Keyboard** (Tabs group), or with `export_tab` in the [`[keys]` table](#the-keys-table).

A Save dialog opens with a name like `zsh 2026-10-24 15.30.txt` (the tab's title and the local time). The extension you save with picks the format:

| Extension | Format |
| --- | --- |
| `.txt` | Plain text (UTF-8). |
| `.txt.gz` | Plain text, gzip-compressed. |
| `.ansi` | With colors: colors (including 256-color and true color) and bold, dim, italic, underline, inverse and strikethrough as escape sequences that `less -R` and `cat` show in a terminal. |
| `.ansi.gz` | With colors, gzip-compressed (`zless -R` reads it). |

Lines the terminal wrapped to fit the window are joined back into one line, and spaces at the end of a line are dropped. The dialog offers one file type for all four formats: type the extension you want (a name without one is saved as plain text). Saving over an existing file replaces it only once the export is complete.

When the tab is showing a full-screen program (vim, less, top), the file holds the shell's history behind it, not the program's screen; the dialog title and the notice say so. Nothing a program prints can start an export.

When the file is written, a notice under the tab bar names it, with a button that shows it selected in Finder (in Explorer on Windows; on Linux it opens the folder in your file manager). An error, such as a folder you cannot write to or a full disk, shows in the same place, as does a note when an export is already running or the tab has nothing to export yet. The tab keeps running and the window stays responsive while a long history is written; resizing the window during an export stops it with a message, so try again.
