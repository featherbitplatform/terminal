---
id: intro
slug: /
title: featherbit terminal
sidebar_label: Introduction
---

featherbit terminal (`fbterm`) is a native terminal emulator built around SSH. It is written in Rust, draws its own interface on the GPU, and does not embed a web view, so it keeps memory and CPU use modest with many sessions open: the app itself takes about 100 MB, and an idle tab adds a fraction of a megabyte plus its scrollback.

## What it is for

- **Many sessions at once.** Idle tabs cost little. Each tab is a local shell or an SSH session, and the layout you had open comes back the next time you launch.
- **SSH as a first-class citizen.** Saved connection profiles carry the host, user, port and how to authenticate. Several tabs on the same profile share one SSH connection.
- **Jump hosts built in.** A profile can name one or more other profiles to hop through, so reaching a machine behind a bastion is one click or one palette entry.
- **Keyboard first.** A palette (`Cmd+Shift+P` on macOS) finds any profile or action by typing a few letters.
- **Secrets stay out of your files.** Passwords and key passphrases live in the system keychain or an encrypted vault of your choice (see [Saved secrets](saved-secrets)). Your profiles file contains none.

## How it is configured

Everything you set up is plain TOML in one directory, written atomically and reloaded when it changes on disk, so you can keep it in version control or edit it by hand.

- [Profiles and jump hosts](profiles) describes `profiles.toml`.
- [Where files live](files) lists every file, its location on each system, and the settings you can change.
- [Keyboard shortcuts](keyboard-shortcuts) lists every shortcut.

## Platforms

macOS is the first supported platform and ships as a universal disk image. See [Install on macOS](install-macos). The code base also builds for Linux and Windows; the shortcut and file location pages cover those systems too.
