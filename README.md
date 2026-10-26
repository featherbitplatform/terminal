# featherbit terminal

A native, SSH-first terminal emulator for macOS, written in Rust. It stays fast and light with many sessions open: twenty tabs with full scrollback take about 123 MB of memory.

![CI](https://github.com/featherbitplatform/terminal/actions/workflows/ci.yml/badge.svg) ![License](https://img.shields.io/badge/license-Apache--2.0-blue)

![The quick-launch palette over a local shell, listing SSH profiles with their jump hosts](docs/images/palette.png)

## Documentation

Full documentation: [featherbitplatform.github.io/terminal](https://featherbitplatform.github.io/terminal/). Source in [`website/`](website/) (Docusaurus); run it locally with `cd website && npm install && npm start`.

## Install

Download the `.dmg` from the [latest release](https://github.com/featherbitplatform/terminal/releases/latest). It is a universal build for Apple Silicon and Intel Macs. Builds are not signed yet, so macOS asks you to confirm the first launch: see [Install on macOS](https://featherbitplatform.github.io/terminal/install-macos) for the steps and for checking the download's SHA-256.

## Features

- **SSH profiles**: password, private key (with or without passphrase) and ssh-agent authentication, jump hosts through other saved profiles, one shared connection per profile. Host keys are checked against `~/.ssh/known_hosts`, with a prompt for unknown hosts; changed keys are refused.
- **Secrets in the keychain**: passwords and passphrases live in the macOS keychain, never in config files.
- **Quick-launch palette** (Cmd+Shift+P): fuzzy search over profiles and app actions.
- **Tabs and windows**: multiple windows, per-profile tab colors, layout restored on relaunch.
- **Find**: in a tab's scrollback or across all tabs; export a tab's history to a file.
- **Compact scrollback**: history is stored compactly; the `scrollback_lines` setting applies to open tabs.
- **Settings** window and a plain `settings.toml` that reloads live; configurable keyboard shortcuts.
- **GPU rendering** with the Graphite theme; redraws are paced and hidden tabs do no render work.

Local shells work too: shells from `/etc/shells` are offered automatically.

## Build from source

Requires a recent stable Rust toolchain (edition 2024).

```bash
cargo run -p fbterm            # build and run
cargo test --workspace         # tests
bash dev/package-macos.sh 0.1.0  # universal .app and .dmg into target/dist/ (macOS only)
```

The code builds and tests on macOS, Linux and Windows; release builds are macOS only for now. Terminal emulation comes from [feathercritty](https://github.com/featherbitplatform/feathercritty), our fork of the `alacritty_terminal` crate.

## Contributing

Work happens on `feature/*` branches off `develop` with [Conventional Commits](https://www.conventionalcommits.org/); `main` holds tagged releases only. CI runs `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`, `cargo deny check` and live SSH tests.

## License

Apache-2.0, see [`LICENSE`](LICENSE). Third-party licenses are listed in [`LICENSES.md`](LICENSES.md).
