# Licenses

fbterm itself is licensed under Apache-2.0 (see `LICENSE`).

## Third-party licenses

`deny.toml` accepts only the licenses below; `cargo deny check` fails on anything else. Crates with several license options (for example MIT or Apache-2.0) only need one of them to be on this list.

| License | Where it appears |
| --- | --- |
| Apache-2.0 | Most of the dependency tree, including `russh`, `alacritty_terminal`, `iced` support crates and `wgpu`. |
| MIT | The largest share of the tree, including `iced`, `tokio` and `portable-pty`. |
| BSD-2-Clause | `arrayref`, `serial2`, `zerocopy`. |
| BSD-3-Clause | `curve25519-dalek`, `ed25519-dalek`, `subtle`, `tiny-skia`, `num_enum`. |
| ISC | `ring`, `untrusted`, `libloading`, `inotify`. |
| Unicode-3.0 | `unicode-ident` (Unicode data tables). |
| Zlib | `bytemuck`, `foldhash`, `slotmap`, `tinyvec`, the `objc2-*` bindings, `raw-window-handle`. |
| MPL-2.0 | `nucleo-matcher` (palette fuzzy search) and `option-ext`; used unmodified. |
| OFL-1.1 | The bundled fonts, see below. |
| BSL-1.0 | Windows builds only, see below. |
| CC0-1.0 | `hexf-parse` and `notify`, see below. |

### BSL-1.0 and CC0-1.0

- BSL-1.0 is used only by Windows builds, through `clipboard-win` and `error-code`, which `iced_winit` pulls in for Windows clipboard support (`iced_winit` > `window_clipboard` > `clipboard-win` > `error-code`).
- CC0-1.0 comes from `hexf-parse`, used by the `naga` shader compiler that `wgpu` depends on, and from `notify`, the file-watching crate fbterm uses directly.

## Bundled fonts

Geist and Geist Mono are bundled under the SIL Open Font License 1.1. The license text is in `crates/fbterm/assets/fonts/LICENSE`.

## Security advisory exception

`deny.toml` ignores RUSTSEC-2023-0071 (the Marvin timing side channel in the `rsa` crate, which has no fixed release). RSA user keys stay supported, and the app warns when one is used to log in. Verifying a server's RSA host key is not affected.
