---
id: install-macos
title: Install on macOS
sidebar_label: Install on macOS
---

featherbit terminal is distributed as a universal disk image that runs natively on both Apple silicon and Intel Macs.

## Download

Each release provides two files:

- `fbterm-<version>-macos-universal.dmg`, the disk image
- `fbterm-<version>-macos-universal.dmg.sha256`, its checksum

For example, version 0.6.0 is `fbterm-0.6.0-macos-universal.dmg`.

## Verify the download

Put both files in the same folder and run this from that folder:

```bash
shasum -a 256 -c fbterm-0.6.0-macos-universal.dmg.sha256
```

The command prints `fbterm-0.6.0-macos-universal.dmg: OK` when the file is intact. Replace the version with the one you downloaded.

## Install

1. Open the `.dmg`.
2. Drag **featherbit terminal.app** onto the **Applications** shortcut in the same window.
3. Eject the disk image.

## First launch

The app carries an ad-hoc signature, which Apple silicon requires, but it is not signed with a Developer ID and is not notarized. The first time you open it, macOS says it cannot verify the app and offers only **Done** or **Move to Trash**. Choose **Done**, then approve the app once in one of these ways.

**Option 1: System Settings.** Open **System Settings**, then **Privacy & Security**, scroll down to the message about featherbit terminal and click **Open Anyway**. Confirm with your password or Touch ID, then click **Open Anyway** again in the dialog that follows.

**Option 2: clear the quarantine flag.** Run this once in another terminal:

```bash
xattr -dr com.apple.quarantine "/Applications/featherbit terminal.app"
```

After either option the app opens normally from then on. On macOS 15 and later, right-clicking the app and choosing **Open** no longer skips this check; use one of the options above.

## If fbterm crashes or doesn't start

fbterm keeps a log of each run in `fbterm.log` in its [configuration directory](files#configuration-directory), `~/Library/Application Support/io.featherbit.fbterm`. When it starts, the log of the run before is kept as `fbterm.previous.log`, so after a crash that file holds the run that crashed. A crash, or a start that fails, also adds a report to `crash.log` in the same folder (once that file is over 256 KB, it is moved to `crash.previous.log` first), and the next launch says once, in the window, that a report was written. In Finder, choose **Go** > **Go to Folder** and paste the folder's path to open it.

To report the problem, send `crash.log` and `fbterm.previous.log` (or `fbterm.log` if fbterm has not been started again since). Look through them first: they can name your profiles, hosts and folders.

## Next steps

On first launch you get a local shell. Add your SSH hosts as [profiles](profiles) and open them from the palette with `Cmd+Shift+P`.
