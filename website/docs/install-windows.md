---
id: install-windows
title: Install on Windows
sidebar_label: Install on Windows
---

featherbit terminal is distributed for Windows 11 (x64) as an `.msi` installer.

## Download

Each release provides two files:

- `fbterm-<version>-windows-x86_64.msi`, the installer
- `fbterm-<version>-windows-x86_64.msi.sha256`, its checksum

For example, version 0.6.2 is `fbterm-0.6.2-windows-x86_64.msi`.

## Verify the download

Put both files in the same folder and run this in PowerShell from that folder:

```powershell
(Get-FileHash .\fbterm-0.6.2-windows-x86_64.msi -Algorithm SHA256).Hash
Get-Content .\fbterm-0.6.2-windows-x86_64.msi.sha256
```

The first command prints the checksum of the file you downloaded and the second prints the one published with the release; the hash at the start of that line must be the same (PowerShell prints it in capital letters, the file in lowercase, which is the same value). Replace the version with the one you downloaded.

## Install

Double-click the `.msi` and follow the installer. It installs for all users of the machine, so Windows asks for administrator approval, and puts the program in `C:\Program Files\featherbit terminal`. It also adds a Start menu shortcut named **featherbit terminal**.

The installer is not signed yet, so on the first run Windows SmartScreen may show **Windows protected your PC**. Choose **More info**, then **Run anyway**.

To install from a prompt without the wizard, run `msiexec /i fbterm-0.6.2-windows-x86_64.msi /qb` as administrator.

## Starting and checking the version

Open **featherbit terminal** from the Start menu. The program has no console window, so `fbterm --version` in a release build prints only when its output goes somewhere other than the console, for example:

```powershell
& "C:\Program Files\featherbit terminal\fbterm.exe" --version | more
```

## Upgrade and uninstall

Installing a newer `.msi` replaces the older version in place. To uninstall, use **Settings**, **Apps**, **Installed apps**, find **featherbit terminal** and choose **Uninstall**.

Uninstalling leaves your configuration directory, `%APPDATA%\featherbit\fbterm\config`, in place: your profiles, settings, and `secrets.dpapi` with your saved passwords and passphrases (encrypted for your Windows account) all survive, so a reinstall picks up where you left off. Delete that folder to remove them. See [Where files live](files).

## Window controls

fbterm draws its own minimize, maximize and close buttons in the tab bar in the style of Windows 11. Drag an empty part of the bar to move the window, double-click it to maximize or restore, and drag any edge or corner to resize.

## Blur

`background_blur` uses the translucent window backdrop of Windows 11 22H2 and newer (build 22621 and later). On older builds the switch in Settings is disabled and reads `Not supported on this system`. A window can only blur if it was opened while `background_opacity` is below `1.0`.

## Saved secrets

Passwords and key passphrases are kept in `secrets.dpapi`, encrypted for your Windows account, or in the encrypted vault if you choose it. See [Saved secrets](saved-secrets).

## If fbterm crashes or doesn't start

fbterm keeps a log of each run in `fbterm.log` in its [configuration directory](files#configuration-directory), `%APPDATA%\featherbit\fbterm\config`. When it starts, the log of the run before is kept as `fbterm.previous.log`, so after a crash that file holds the run that crashed. A crash, or a start that fails, also adds a report to `crash.log` in the same folder (once that file is over 256 KB, it is moved to `crash.previous.log` first), and the next launch says once, in the window, that a report was written. If fbterm cannot start, a message says so and names the report's file. To open the folder, paste its path into the File Explorer address bar.

To report the problem, send `crash.log` and `fbterm.previous.log` (or `fbterm.log` if fbterm has not been started again since). Look through them first: they can name your profiles, hosts and folders.

## Next steps

On first launch you get a local shell. Add your SSH hosts as [profiles](profiles) and open them from the palette with `Ctrl+Shift+P`.
