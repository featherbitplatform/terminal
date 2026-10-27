# File panel: design

Status: approved by the owner, 2026-10-27. Milestone 4 of
`2026-10-23-featherbit-terminal-design.md` (section 5.8), which this document refines. Where they
differ, this document wins for the file panel.

## 1. Intent

The owner moves files between their machine and servers mostly for two jobs: pulling down
exported, compressed log archives, and pushing up configuration files. Today that means a
separate tool or remembering paths for a command. The file panel puts a browser of the server's
filesystem next to the session, over the connection that is already open, so finding a log
archive and saving it, or dropping a config file into place, takes a few clicks.

Success: in a session on a server, the owner opens the panel, starts in the home folder, walks
to `/var/log/app/exports`, types part of the archive's name to narrow the list, clicks
Download, and finds the file in Downloads; then uploads a config file into `/etc/app/`, is asked
before it replaces the existing one, and renames the upload instead.

## 2. Goals

1. A **side panel** docked beside the terminal (the terminal stays visible), for the focused
   pane's SSH profile, over SFTP on the profile's existing shared connection: no extra login or
   prompt, jump hosts included.
2. **Browsing:** starts in the remote home folder; the user can go up and anywhere in the
   filesystem; a path bar that can be clicked (each segment) or typed into; a list with name,
   size and modified time, sortable by each; a **filter box** that narrows the current folder
   by name as you type.
3. **Downloading:** `Download` saves straight to the Downloads folder; `Download as…` opens a save
   dialog to choose the folder, file name and extension.
4. **Uploading:** `Upload…` picks files on this machine and puts them in the current server
   folder; dragging files from Finder onto the panel does the same.
5. **Name clashes on upload:** fbterm asks first, offering `Replace`, `Rename` (type a new name)
   or `Cancel`.
6. **Transfers** show progress with a cancel button and never block the terminal; a finished
   download offers `Show in Finder` (`Show in folder` elsewhere).

All of the above was described or confirmed by the owner on 2026-10-27 (the filter box,
progress, drag-in and the panel's toggle were proposed and accepted with "nothing is missing").

## 3. Non-goals and anti-goals

- Not in this version (owner-confirmed): editing remote files in place, changing permissions or
  ownership, deleting or renaming remote files, creating folders, syncing folders, and dragging
  files out of the panel to Finder (main design 5.8 listed some of these as builder's calls;
  they are deferred).
- No local file browser; this machine's side is the system's own dialogs and Finder.
- Anti-goals, a failure even if the panel works:
  - A remote program can never trigger a transfer, open the panel, or write anywhere on this
    machine. Downloads happen only on the user's click, to the folder the user chose (or
    Downloads).
  - A download never overwrites a local file silently: `Download` into Downloads picks a free
    name (`name (1).ext`), and `Download as…` lets the system dialog handle replacement.
  - An upload never replaces a remote file without the user saying so.
  - A slow or huge transfer never freezes the terminal, the panel's browsing, or other tabs.
  - Remote file names are shown as they are, never interpreted (no escape sequences rendered).

## 4. Behaviour

**Opening.** A shortcut and a palette action (`File panel`) toggle the panel for the focused
pane; it is available only when that pane's profile is SSH and connected. The panel remembers
its width. In a split tab it belongs to the tab and follows the focused pane's profile; switching
focus to a pane of another profile switches the panel to that profile's server, keeping each
profile's last folder for the session.

**Browsing.** Folders first, then files, by name by default; clicking a column header sorts by
it (again reverses). Double-click or Enter opens a folder; a `..` row and Backspace go up.
Symlinks show their target and open as what they point to. The filter box filters the loaded
folder only (no recursive search). Errors (permission denied, missing folder) show inline in the
panel with the server's message.

**Transfers.** A queue at the bottom of the panel, one row per transfer: name, direction, bytes
done of total, a progress bar, and cancel. Several transfers may run; the queue runs them over
the shared connection. A cancelled or failed download removes its partial local file; a
cancelled or failed upload removes its partial remote file only if fbterm created it (it uploads
to a temporary name in the same folder and renames on completion, so an existing file is never
left half-replaced). Finished rows stay until dismissed or the panel closes.

**Clash dialog.** When an uploaded file's name exists: `"{name}" already exists in {folder}.`
with `Replace`, `Rename`, `Cancel`; `Rename` shows a name field prefilled with `name (1).ext`.
With several files, the dialog has `Apply to all` for Replace and Cancel.

**Settled after the builder check** *(my calls, 2026-10-27; the owner can change any of them)*:

- **Replace keeps the file's mode and owner.** An upload goes to a temporary name in the same
  folder, gets the existing file's permissions (and owner and group when the server allows it),
  and replaces the target with an atomic rename (`posix-rename@openssh.com`). When that is not
  possible (the folder isn't writable, or the server lacks the extension), Replace writes the
  existing file in place instead and the transfer row says `Replaced in place`; a cancelled or
  failed in-place replace leaves the row red with `File may be incomplete`. A brand-new upload
  gets the server's default permissions.
- **Downloads are safe too:** a download writes to a temporary file next to its destination and
  renames on completion, so a cancelled or failed download leaves nothing behind and never
  damages a file the user agreed to replace. A symlink to a file downloads its target; special
  files (devices, sockets, pipes) and broken links can't be downloaded; a size that changes
  during the download is not an error.
- **Transfers belong to the profile's connection, not the panel.** Hiding the panel or moving
  focus never cancels them; the queue reappears when the panel opens for that profile again.
  Closing a tab or window, or quitting, while transfers of that connection run asks first
  (`{n} transfers are still running.` with `Cancel transfers and close` / `Keep open`). If the
  connection drops, running transfers fail with the reason and offer `Retry`, which reconnects
  like Enter in a shell.
- **The panel follows the focused pane.** It works for any SSH profile whose connection is up,
  jump-host profiles included; on a local pane, or an SSH pane that isn't connected, it shows
  `Not connected` (or `Local session`) instead of a listing, and its transfers keep running.
- **Selection:** one item at a time in the listing; Download works on files, not folders.
  Uploads take several files at once (the clash dialog's `Apply to all` covers them); folders
  are not uploaded (a dropped or picked folder is skipped with a note in the queue).
- **Drag and drop:** a drop anywhere in the window uploads to the panel's current folder while
  the panel is open; with the panel closed, drops are ignored.
- **SFTP unavailable:** an inline error with the server's message; no fallback. The home folder
  is the server's answer for `.` (in an SFTP-only chroot that may be `/`).
- **Hidden files** are shown, with a `Show hidden files` toggle (on by default) remembered per
  app.

## 5. Constraints

- Everything in the main design and `CLAUDE.md` holds: in-process SSH only (`russh-sftp` on the
  shared connection, main design 5.3; the connection stays open while the panel is open),
  cross-platform (macOS first: Finder, Downloads), Graphite look, no naming of other terminal
  applications, secrets untouched, Conventional Commits with no attribution.
- Redraw contract: transfer progress updates are coalesced (a few per second at most); no
  periodic loops; a hidden panel does no work except transfers.
- Live SSH tests (Docker) cover listing, download, upload with a clash, and cancel, through a
  jump host.

## 6. Left to the builder

- Panel default width and bounds, the shortcut's default chord, list row layout and icons.
- Transfer chunk sizes and how many run at once.
- How the panel coexists with split panes' layout (it docks at the tab's right edge, outside the
  panes).
- Whether the filter matches substrings or fuzzily (substring is the safer default for file
  names).
