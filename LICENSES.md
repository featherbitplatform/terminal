# Licenses

fbterm itself is licensed under Apache-2.0 (see `LICENSE`).

## Third-party licenses

`deny.toml` accepts only the licenses below; `cargo deny check` fails on anything else. Crates with several license options (for example MIT or Apache-2.0) only need one of them to be on this list.

| License | Where it appears |
| --- | --- |
| Apache-2.0 | Most of the dependency tree, including `russh`, `russh-sftp` (the file panel's SFTP client), `feathercritty`, `iced` support crates and `wgpu`; also `age` (profile archives, MIT or Apache-2.0) with its RustCrypto dependencies (`scrypt`, `chacha20poly1305`, `secrecy`), `zeroize` and `base64`; and `windows-sys` (MIT or Apache-2.0), used directly on Windows for DPAPI, the system storage for saved secrets there. |
| MIT | The largest share of the tree, including `iced`, `tokio` and `portable-pty`; also `i18n-embed`, `rust-embed` and `bech32`, which `age` uses for its own messages and key encoding; also the Lucide icons derived from Feather (see below); also `secret-service` and `zbus`, already in the tree through `keyring` and used directly on Linux only to recognize a missing Secret Service; also `winresource`, a build-time tool used only by Windows builds (`crates/fbterm/build.rs`) to embed the icon and version information in `fbterm.exe`; nothing of it ships in the binary. |
| BSD-2-Clause | `arrayref`, `serial2`, `zerocopy`. |
| BSD-3-Clause | `curve25519-dalek`, `ed25519-dalek`, `x25519-dalek` (via `age`), `subtle`, `tiny-skia`, `num_enum`. |
| ISC | `ring`, `untrusted`, `libloading`, `inotify`, and the bundled Lucide icon font (see below). |
| Unicode-3.0 | `unicode-ident` (Unicode data tables); `tinystr`, `zerovec` and `zerofrom` (via `age`'s message localization). |
| Zlib | `bytemuck`, `foldhash`, `slotmap`, `tinyvec`, the `objc2-*` bindings, `raw-window-handle`. |
| MPL-2.0 | `nucleo-matcher` (palette fuzzy search) and `option-ext`; used unmodified. |
| OFL-1.1 | The bundled fonts, see below. |
| BSL-1.0 | Windows builds only, see below. |
| CC0-1.0 | `hexf-parse` and `notify`, see below. |

### BSL-1.0 and CC0-1.0

- BSL-1.0 is used only by Windows builds, through `clipboard-win` and `error-code`, which `iced_winit` pulls in for Windows clipboard support (`iced_winit` > `window_clipboard` > `clipboard-win` > `error-code`).
- CC0-1.0 comes from `hexf-parse`, used by the `naga` shader compiler that `wgpu` depends on, and from `notify`, the file-watching crate fbterm uses directly.

## Gallery themes

Color schemes installed from the theme gallery are not part of fbterm and are not shipped with it. They come from [terminal-themes](https://github.com/featherbitplatform/terminal-themes): its catalog, which holds every theme in full, is downloaded only when you open the gallery or press Refresh or Retry, and Install writes the chosen theme from that catalog. Each one is under its own permissive license, stated together with its project and author in its file (the `[credit]` table) and in `terminal-themes`, where the full credits are kept.

## feathercritty

The terminal emulation library, [feathercritty](https://github.com/featherbitplatform/feathercritty), is our fork of [`alacritty_terminal`](https://github.com/alacritty/alacritty/tree/master/alacritty_terminal) 0.26.0 by the Alacritty authors (Apache-2.0, license in the fork's `LICENSE-APACHE`; attribution required). It is used as a git dependency pinned by commit. The fork's changes (a bug fix, a scroll counter, a read-only getter and the rename) are listed in its README, and the files it modifies say so in their first line.

## winit

iced's windowing library, [winit](https://github.com/rust-windowing/winit) 0.30.13 by the winit authors (Apache-2.0), comes from our fork, [featherbitplatform/winit](https://github.com/featherbitplatform/winit), through a `[patch.crates-io]` entry pinned by commit in the root `Cargo.toml`; with it comes the `dpi` crate of the same release (Apache-2.0 or MIT). The fork's one change, a backport of upstream winit's Windows 11 `WM_DPICHANGED` fix (upstream commit `488c036`, issue rust-windowing/winit#4041: a window dragged onto a monitor with a different scale keeps the position Windows suggests), is described in its `FORK.md`, and the files it modifies say so in their first line.

## Bundled fonts

Geist and Geist Mono are bundled under the SIL Open Font License 1.1. The license text is in `crates/fbterm/assets/fonts/LICENSE`.

The icons are glyphs of the Lucide icon font (`lucide.ttf` from `lucide-static` 1.54.0), subset to the icons the app uses by `dev/subset-icons.sh` into `crates/fbterm/assets/fonts/lucide-icons.ttf`. Lucide is under the ISC License; the icons it derives from Feather (among those bundled: `x`, `plus`, `chevron-up`, `chevron-down`, `lock`) are under the MIT License. Both texts are in `crates/fbterm/assets/fonts/LICENSE-lucide`.
