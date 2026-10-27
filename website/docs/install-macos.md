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

For example, version 0.3.0 is `fbterm-0.3.0-macos-universal.dmg`.

## Verify the download

Put both files in the same folder and run this from that folder:

```bash
shasum -a 256 -c fbterm-0.3.0-macos-universal.dmg.sha256
```

The command prints `fbterm-0.3.0-macos-universal.dmg: OK` when the file is intact. Replace the version with the one you downloaded.

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

## Next steps

On first launch you get a local shell. Add your SSH hosts as [profiles](profiles) and open them from the palette with `Cmd+Shift+P`.
