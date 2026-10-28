---
id: install-linux
title: Install on Linux
sidebar_label: Install on Linux
---

featherbit terminal ships for Linux as a `.deb`, an `.rpm` and a `.tar.gz`, each for x86-64 (`x86_64`) and 64-bit Arm (`aarch64`). Pick the one that matches your system.

| Package | Supported systems |
| --- | --- |
| `.deb` | Debian 12 and newer, Ubuntu 22.04 and newer, and distributions based on them |
| `.rpm` | Fedora 39 and newer, RHEL 9 and newer, and compatible systems (Rocky Linux, AlmaLinux) |
| `.tar.gz` | Any other distribution with glibc 2.34 or newer |

You need a desktop session, Wayland or X11, with Vulkan or OpenGL graphics.

## Download

Each release provides, for each architecture and format, the package and its checksum. For example, for version 0.6.0 on x86-64:

- `fbterm-0.6.0-linux-x86_64.deb` and `fbterm-0.6.0-linux-x86_64.deb.sha256`
- `fbterm-0.6.0-linux-x86_64.rpm` and `fbterm-0.6.0-linux-x86_64.rpm.sha256`
- `fbterm-0.6.0-linux-x86_64.tar.gz` and `fbterm-0.6.0-linux-x86_64.tar.gz.sha256`

In the commands on this page, replace the example version 0.6.0 and `x86_64` with the ones you downloaded (`aarch64` on Arm machines).

## Verify the download

Put the package and its `.sha256` file in the same folder and run this from that folder:

```bash
sha256sum -c fbterm-0.6.0-linux-x86_64.deb.sha256
```

It prints `fbterm-0.6.0-linux-x86_64.deb: OK` when the file is intact.

## Install

On Debian, Ubuntu and their relatives:

```bash
sudo apt install ./fbterm-0.6.0-linux-x86_64.deb
```

On Fedora, RHEL and compatible systems:

```bash
sudo dnf install ./fbterm-0.6.0-linux-x86_64.rpm
```

Both put the program at `/usr/bin/fbterm`, add **featherbit terminal** to your application launcher (a desktop entry in the System and Terminal Emulator categories) and install the icon. Your package manager also installs the libraries fbterm needs: xkbcommon, the Wayland and X11 client libraries and D-Bus.

### From the tarball

Unpack the archive; it holds `bin/fbterm`, `share/applications/fbterm.desktop`, the icons under `share/icons/hicolor/` and an `INSTALL` note with these commands. For your user only:

```bash
tar -xzf fbterm-0.6.0-linux-x86_64.tar.gz
cd fbterm-0.6.0-linux-x86_64
install -Dm755 bin/fbterm ~/.local/bin/fbterm
cp -r share/applications share/icons ~/.local/share/
```

For every user, install `bin/fbterm` into `/usr/local/bin` and copy `share/applications` and `share/icons` into `/usr/local/share` with `sudo`. Make sure `~/.local/bin` is on your `PATH` for a per-user install.

Check the install with `fbterm --version`.

## Graphics

fbterm draws on the GPU through Vulkan or OpenGL (EGL). The packages recommend the loaders, so they normally arrive with the install; if the window does not open, install them and a driver for your graphics card:

- Debian and Ubuntu: `libvulkan1`, `libegl1`, `libwayland-egl1`, plus Mesa drivers (`mesa-vulkan-drivers`) or your vendor's driver.
- Fedora and RHEL: `vulkan-loader`, `mesa-libEGL`, `libwayland-egl`, plus `mesa-vulkan-drivers` or your vendor's driver.

## Title bar

On Linux fbterm draws the window's title bar itself, in the tab bar: minimize, maximize and close buttons that follow your desktop's button layout (GNOME and KDE settings are read; otherwise the three buttons sit on the right), drag on an empty part of the bar to move the window, double-click it to maximize, and drag any edge or corner to resize.

If you prefer the desktop's own title bar, choose **System** under **Title bar** in Settings, or set the `window_decorations` setting to `"system"`:

```toml
window_decorations = "system"
```

The default is `"fbterm"`. The setting is described in [Where files live](files#settingstoml).

Some desktops refuse to let an application move or resize its own window. When fbterm sees that, it switches that session to the system title bar and shows this notice once:

> This desktop doesn't let fbterm move its window, so the system title bar is used. Change it in Settings.

The setting itself is not changed, so choosing **fbterm** under **Title bar** in Settings tries the drawn title bar again.

## Blur

`background_blur` (see [Where files live](files#settingstoml)) works on KDE Plasma under Wayland. Elsewhere the switch in Settings is disabled and reads `Not supported on this system`. A window can only blur if it was opened while `background_opacity` is below `1.0`.

## Saved secrets

Passwords and key passphrases are kept in the desktop's Secret Service (GNOME Keyring, KWallet and compatible programs) when one is running. On a desktop without one, fbterm switches to the encrypted vault, a file protected by a main password you choose. See [Saved secrets](saved-secrets) for how the choice works and what to do if you forget the main password.

## Uninstall

```bash
sudo apt remove fbterm     # Debian, Ubuntu
sudo dnf remove fbterm     # Fedora, RHEL
```

For a tarball install, delete the files you copied (`fbterm` from `bin`, `fbterm.desktop` from `share/applications` and `fbterm.png` from each `share/icons/hicolor/<size>/apps`). Removing the program leaves your [configuration directory](files#configuration-directory), `~/.config/fbterm`, in place; delete it too to remove your profiles and settings.

## If fbterm crashes or doesn't start

fbterm keeps a log of each run in `fbterm.log` in its [configuration directory](files#configuration-directory), `~/.config/fbterm` (or `$XDG_CONFIG_HOME/fbterm`). When it starts, the log of the run before is kept as `fbterm.previous.log`, so after a crash that file holds the run that crashed. A crash, or a start that fails, also adds a report to `crash.log` in the same folder (once that file is over 256 KB, it is moved to `crash.previous.log` first), and the next launch says once, in the window, that a report was written.

To report the problem, send `crash.log` and `fbterm.previous.log` (or `fbterm.log` if fbterm has not been started again since). Look through them first: they can name your profiles, hosts and folders.

## Next steps

On first launch you get a local shell. Add your SSH hosts as [profiles](profiles) and open them from the palette with `Ctrl+Shift+P`.
