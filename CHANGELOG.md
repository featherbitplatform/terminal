# Changelog

All notable changes to featherbit terminal (`fbterm`). The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html). Each version links to its GitHub release, where the downloads are.

## [0.6.4] - 2026-10-30

### Fixed

- Settings: the unit after a typed number stays inside its card whatever its length. The "attempts" label of the Tunnels card's reconnect setting ran past the card's edge ([#51](https://github.com/featherbitplatform/terminal/pull/51)).

## [0.6.3] - 2026-10-29

### Fixed

- Every rule is drawn one physical pixel wide in every window, on every screen scale ([#48](https://github.com/featherbitplatform/terminal/pull/48)).
- The outline strip under the drawn window controls (Windows and Linux) no longer shows while a panel is docked ([#48](https://github.com/featherbitplatform/terminal/pull/48)).

## [0.6.2] - 2026-10-29

### Fixed

- Single lines where the sidebar meets the header and the footer in the Color schemes and profile editor windows ([#45](https://github.com/featherbitplatform/terminal/pull/45)).
- The content outline stays under the drawn window controls on Windows and Linux ([#45](https://github.com/featherbitplatform/terminal/pull/45)).

## [0.6.1] - 2026-10-28

### Fixed

- The content outline stays when no panel is docked ([#42](https://github.com/featherbitplatform/terminal/pull/42)).

## [0.6.0] - 2026-10-28

### Added

- The Copy/Paste menu opens on right-click on macOS too. Every right-click opens it and keeps the selection, with Copy enabled when something is selected; Windows and Linux keep copy-on-right-click ([#36](https://github.com/featherbitplatform/terminal/pull/36)).

### Changed

- Tunnels live in a panel docked at the right edge of the window, like the file panel, instead of a separate Tunnels window. Opening one panel closes the other, and the focused pane's profile group is highlighted ([#39](https://github.com/featherbitplatform/terminal/pull/39)).
- The active tab and the content are outlined while a panel is docked ([#37](https://github.com/featherbitplatform/terminal/pull/37)).
- Windows: window placement when the screen scale changes follows upstream's Windows 11 fix, carried in our winit fork ([#38](https://github.com/featherbitplatform/terminal/pull/38)).

## [0.5.0] - 2026-10-28

### Added

- Theme gallery: ready-made color schemes from `featherbitplatform/terminal-themes`, installed and updated from the Color schemes window with their credits. The catalog is fetched only when the Gallery tab is opened ([#32](https://github.com/featherbitplatform/terminal/pull/32)).
- Optional update check, asked once at first launch. While on, the app asks GitHub once a day for a newer version and shows a dot on the settings button; nothing is downloaded ([#31](https://github.com/featherbitplatform/terminal/pull/31)).

### Fixed

- Windows: a window dragged between monitors of different scale stays under the cursor ([#30](https://github.com/featherbitplatform/terminal/pull/30)).

## [0.4.0] - 2026-10-28

### Added

- A run log and crash reports in the config directory ([#23](https://github.com/featherbitplatform/terminal/pull/23)), with window moves, scale changes and resizes logged ([#25](https://github.com/featherbitplatform/terminal/pull/25)).
- Files dropped on a terminal type their paths ([#26](https://github.com/featherbitplatform/terminal/pull/26)).
- After an SSH shell connects, the app checks SFTP in the background and the tab bar shows a files button while it works ([#27](https://github.com/featherbitplatform/terminal/pull/27)).

### Fixed

- Saved-secret writes started from the profile editor complete even after the editor is closed ([#24](https://github.com/featherbitplatform/terminal/pull/24)).

## [0.3.1] - 2026-10-28

### Fixed

- macOS: dragging a tab no longer moves the window ([#20](https://github.com/featherbitplatform/terminal/pull/20)).
- Windows: numpad digits are read as digits with Num Lock on ([#20](https://github.com/featherbitplatform/terminal/pull/20)).
- The shortcut recorder says when the system keeps a key press for itself ([#20](https://github.com/featherbitplatform/terminal/pull/20)).

### Added

- `FBTERM_LOG_KEYS=1` logs keyboard events while the Settings window records a shortcut, for keyboard reports ([#20](https://github.com/featherbitplatform/terminal/pull/20)).

## [0.3.0] - 2026-10-27

### Added

- Right-click copy and paste, with a Copy/Paste menu on Windows and Linux ([#15](https://github.com/featherbitplatform/terminal/pull/15)).
- The Settings window lists the fixed shortcuts next to the changeable ones, and notes when a background opacity change needs a restart ([#15](https://github.com/featherbitplatform/terminal/pull/15)).
- Duplicate a profile from the profile editor ([#14](https://github.com/featherbitplatform/terminal/pull/14)).
- Security scans in CI and CycloneDX SBOMs of the app and the docs site attached to every release ([#16](https://github.com/featherbitplatform/terminal/pull/16)).

## [0.2.0] - 2026-10-27

The first release with Linux and Windows builds.

### Added

- Split panes: tiling within a tab, per-pane zoom, maximize, rearranging tiles, merging tabs, search across panes, and layout restore ([#6](https://github.com/featherbitplatform/terminal/pull/6)).
- Color schemes: four built-ins, user scheme files, the Color schemes window, a global and a per-profile choice, scheme fonts, and pane accent borders ([#7](https://github.com/featherbitplatform/terminal/pull/7)).
- Tunnels: local, remote and SOCKS5 forwards per SSH profile, started on demand or with the shell, reconnecting with backoff (`tunnel_max_retries`), with a Tunnels window, a tab-bar indicator and palette rows ([#8](https://github.com/featherbitplatform/terminal/pull/8)).
- File panel: SFTP browsing, safe downloads and uploads, a transfer queue, and close and quit confirmation while transfers run ([#9](https://github.com/featherbitplatform/terminal/pull/9)).
- Export and import of profile archives, key profiles without a key file, and the saved-secrets choice between the system storage and an encrypted vault with a main password ([#10](https://github.com/featherbitplatform/terminal/pull/10)).
- Linux (`.deb`, `.rpm`, `.tar.gz` for x86_64 and aarch64) and Windows (x86-64 `.msi`) packages, drawn window controls on both systems, blur on Windows 11 22H2+ and KDE Wayland, and one release workflow publishing all of it with the macOS `.dmg` ([#11](https://github.com/featherbitplatform/terminal/pull/11)).
- A README ([#5](https://github.com/featherbitplatform/terminal/pull/5)).

## [0.1.0] - 2026-10-26

The first release: the daily driver milestone, macOS only.

### Added

- GPU terminal widget on `feathercritty`, local shells, tabs and multiple windows with layout restore ([#1](https://github.com/featherbitplatform/terminal/pull/1)).
- SSH profiles (password, key, agent) with jump hosts, one shared connection per profile, and host-key checks against `~/.ssh/known_hosts` ([#1](https://github.com/featherbitplatform/terminal/pull/1)).
- Profile editor, quick-launch palette, TOML settings, the Graphite theme, and the docs site ([#1](https://github.com/featherbitplatform/terminal/pull/1)).
- Compact scrollback history: a live `scrollback_lines` setting with a Settings confirmation, and memory given back when tabs close ([#2](https://github.com/featherbitplatform/terminal/pull/2)).
- Unsigned macOS `.dmg` packaging and the release workflow ([#1](https://github.com/featherbitplatform/terminal/pull/1)).

[0.6.4]: https://github.com/featherbitplatform/terminal/releases/tag/v0.6.4
[0.6.3]: https://github.com/featherbitplatform/terminal/releases/tag/v0.6.3
[0.6.2]: https://github.com/featherbitplatform/terminal/releases/tag/v0.6.2
[0.6.1]: https://github.com/featherbitplatform/terminal/releases/tag/v0.6.1
[0.6.0]: https://github.com/featherbitplatform/terminal/releases/tag/v0.6.0
[0.5.0]: https://github.com/featherbitplatform/terminal/releases/tag/v0.5.0
[0.4.0]: https://github.com/featherbitplatform/terminal/releases/tag/v0.4.0
[0.3.1]: https://github.com/featherbitplatform/terminal/releases/tag/v0.3.1
[0.3.0]: https://github.com/featherbitplatform/terminal/releases/tag/v0.3.0
[0.2.0]: https://github.com/featherbitplatform/terminal/releases/tag/v0.2.0
[0.1.0]: https://github.com/featherbitplatform/terminal/releases/tag/v0.1.0
