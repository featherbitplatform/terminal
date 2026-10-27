---
id: file-panel
title: File panel
sidebar_label: File panel
---

The file panel browses the server of an SSH session beside the terminal and moves files to and from it over SFTP. It is docked at the right edge of the window; the terminal stays visible and keeps running. Use it to fetch a log, drop a config file onto a host or look around a folder without typing `ls`.

![The file panel beside a terminal: a folder listing, the Download toolbar and a transfer queue](/img/file-panel.png)

## Opening the panel

Press `Cmd+Shift+E` (`Ctrl+Shift+E` on Linux and Windows), choose **File panel** in the palette, or use the close button in the panel's header to hide it again. The `file_panel` action can be rebound in **Keyboard** in Settings (see [Keyboard shortcuts](keyboard-shortcuts#the-keys-table)). Each window has its own panel, shown or hidden; dragging the panel's left edge changes its width, which is shared by all windows and remembered.

The panel follows the focused pane of the active tab and shows that pane's profile:

| Pane | The panel shows |
| --- | --- |
| An SSH pane that is connected | The server's files. |
| An SSH pane that is not connected | `Not connected`. |
| A local shell | `Local session`. |
| A server that refuses SFTP | `SFTP is not available: <reason>`. There is no fallback to SCP or anything else. |

The panel uses the profile's shared connection (and its [jump hosts](profiles#jump-hosts)), so it never asks you to sign in again; if a question does come up (a host key, a password) it appears over the window as it would for a shell. The connection stays open while the panel or a transfer uses it.

A profile's panel starts in the server's home folder. The folder you were in is kept for each profile until you quit the app, so switching between panes and tabs of different servers returns to where you were.

When you open the panel, the keyboard goes to it. `Escape`, a click in the terminal or moving to another pane gives it back; a click in the panel takes it again. While the panel has the keyboard, keys go to the panel only and never to the terminal.

## Browsing

- **Path bar.** Each folder of the current path is a button that goes there. Click the empty end of the bar, or press `/` or `~`, to type a path instead; `Enter` goes there and `Escape` cancels.
- **Opening folders.** `Enter` or a double click opens the selected folder (or a link to one). The `..` row and `Backspace` go up a level.
- **Sorting.** Folders come first, then files. Click the **Name**, **Size** or **Modified** heading to sort by it; click again to reverse.
- **Filter.** The **Filter** box narrows the folder as you type, by part of the name, ignoring case. Any other printable key you type while the panel has the keyboard goes to the filter.
- **Show hidden files** (names starting with a dot) is on by default. Turn it off and the choice is remembered for every panel.
- **Errors** (a folder you cannot read, a dropped connection) appear in the panel with **Retry**, which reopens the session if it is gone and reads the folder again.

File names are shown as plain text. A name that contains control characters or invisible text-direction characters has each of them shown as `?`, so a name cannot disguise itself. A folder is read in the background (`Loading…`) and only the rows on screen are drawn. A folder with more than 100,000 entries is not listed in full: the panel shows `This folder has too many entries to list.`

## Downloading

Select a file and use the toolbar:

- **Download** saves into your Downloads folder. If the name is taken it picks a free one: `report.pdf`, then `report (1).pdf`, `report (2).pdf` and so on. It never replaces anything.
- **Download as…** opens the system's save dialog, offering the remote name, so you pick the folder and name.

A link to a file downloads the file it points to. Folders, special files (devices, sockets) and links that point nowhere cannot be downloaded. A file that grows or shrinks while it downloads is not an error; you get what the server sent until the end of the file.

While a download runs it is written to a part file next to its destination, named `.<name>.fbterm-part`. When it completes the part file is renamed to the final name, so you never see a half-written file under a real name. If you cancel or the download fails, the part file is removed. Two downloads never write to the same destination: names are compared ignoring case, so `Report.pdf` and `report.pdf` count as the same, and a second one to a busy destination is refused with `Already downloading to <name>`.

A finished download has **Show in Finder** (**Show in folder** on Linux and Windows).

## Uploading

- **Upload…** opens a file picker (several files at once).
- **Drag and drop** files from Finder or your file manager onto the window while the panel is open.

Files go into the folder the panel shows. Folders are not uploaded: each one gets a row `Folders are not uploaded: <name>`.

An upload never replaces a file without asking. When the name already exists in the folder, a dialog says `"<name>" already exists in <folder>.` and offers:

- **Replace**: the new content replaces the old file.
- **Rename**: asks for another name, prefilled with a free one such as `report (1).pdf`.
- **Cancel**: skip this file.

With several files, **Apply to all** repeats Replace or Cancel for the rest. A name that is already being uploaded to that folder is skipped with `Already uploading <name>`, and no Replace is offered.

An upload is written to a part file, `.<name>.fbterm-part`, in the target folder and renamed into place when complete, so the server never shows a half-uploaded file under the real name. A new file gets the server's default permissions.

**Replace** keeps the existing file's permissions, and its owner and group when the server allows changing them. It uploads to the part file and swaps it over the old file in one atomic step. Two situations make that impossible: the folder is not writable (you may own the file but not the folder), or the server lacks the `posix-rename@openssh.com` extension or refuses it. The panel then writes the existing file in place and marks the row `Replaced in place`. If a replace in place is cancelled or fails, the row says `File may be incomplete.`, because the old content is already partly overwritten. Replacing a link writes the file it points to and leaves the link as it is.

## The transfer queue

Transfers appear at the bottom of the panel, one row each, with a progress bar:

| Row | Buttons |
| --- | --- |
| Running | **Cancel** |
| Finished download | **Show in Finder** (**Show in folder**) |
| Failed | The reason in red and **Retry**. If the connection dropped, Retry reconnects first (asking again if it needs to) and then repeats the transfer. |
| Finished, failed or skipped | A dismiss button that removes the row. |

Cancelled rows disappear by themselves. Finished and skipped rows are cleared when the panel is closed; failed rows stay until you retry or dismiss them.

Transfers belong to the connection, not to the panel: hiding the panel, switching tabs or moving focus never stops them, and they appear again in the profile's queue when you reopen the panel.

### Closing with transfers running

Closing the last tab or pane of a profile while it has transfers running, closing a window the same way, or quitting asks first: `3 transfers are still running.` (`1 transfer is still running.`) with **Cancel transfers and close** and **Keep open**. Cancelling stops them and removes their part files; quitting waits briefly for that cleanup. While another tab of the same profile stays open, closing a tab does not ask, because the transfers keep running there.

On macOS, `Cmd+Q` asks as well while transfers run. Two limits apply: choosing **Quit** in the app menu with the mouse quits at once without asking, and with every window minimised `Cmd+Q` does nothing until a window is shown again. `Cmd+Q` is also ignored while a dialog is already open.

## Safety

The panel acts only on your clicks and keys. A program running in the terminal, locally or on the server, can never open the panel, start a transfer or make this machine write a file. Names never leave their folder: a remote name is made safe before it is used locally, and an upload name with `/` or control characters is refused.

## Limits

- File names that are not valid UTF-8 appear with replacement characters and cannot be opened or downloaded.
- Only SFTP is used. A server without an SFTP subsystem has no file panel.
- The panel lists, downloads and uploads. It does not create, rename or delete remote files.

## In state.toml

The panel's width is saved as `file_panel_width` and the **Show hidden files** choice as `file_panel_show_hidden = false`, written only once you turn hidden files off. See [state.toml](files#statetoml).
