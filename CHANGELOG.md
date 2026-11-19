# Changelog

All notable changes to featherbit terminal (`fbterm`). The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html). Each version links to its GitHub release, where the downloads are.

## [0.11.0] - 2026-11-19

### Added

- Inline images: programs can show pictures in the terminal with sixel, the kitty graphics protocol (Unicode placeholders included) and OSC 1337, in local shells, over SSH and in local Windows shells through the bundled ConPTY. Pictures are display only, are drawn crisp at the size the program asked for, and stay within memory caps; `Inline images` and `Images in scrollback` in Settings turn them on and off ([#107](https://github.com/featherbitplatform/terminal/pull/107)).
- Reconnect an ended session: a pane whose session ended shows a bar with the reason and a `Reconnect` or `Restart` button, `Cmd+Shift+O` (`Ctrl+Shift+O` on Linux and Windows) brings back every ended pane of the tab, and the tab menu has the same item ([#104](https://github.com/featherbitplatform/terminal/pull/104)).
- The tab bar keeps its Settings, Profiles and Tunnels buttons always visible, and the tabs scroll sideways with the mouse or the arrows that appear when they no longer fit ([#102](https://github.com/featherbitplatform/terminal/pull/102)) by AlessandroVisconti.

### Changed

- Block elements and box-drawing characters (U+2500 to U+259F) are drawn by fbterm to fill their cells instead of coming from the font, so bars, frames and block art join with no gaps at any font size or display scale ([#109](https://github.com/featherbitplatform/terminal/pull/109)), ([#111](https://github.com/featherbitplatform/terminal/pull/111)). Rounded corners and diagonals are drawn the same way, with smooth edges ([#112](https://github.com/featherbitplatform/terminal/pull/112)).
- In the Profiles tab, many save errors at once show the first few and a count of the rest, and the redundant close button is gone ([#102](https://github.com/featherbitplatform/terminal/pull/102)) by AlessandroVisconti.
- The Welcome tab's `Done` button sits right under the questions ([#103](https://github.com/featherbitplatform/terminal/pull/103)).

### Fixed

- Double box-drawing lines join correctly at corners, tees and crosses ([#109](https://github.com/featherbitplatform/terminal/pull/109)).
- A picture size dropped while memory was full comes back by itself once memory frees ([#110](https://github.com/featherbitplatform/terminal/pull/110)).
- A picture sized in pixels or percent with OSC 1337 is drawn at exactly that size, not rounded up to whole cells ([#114](https://github.com/featherbitplatform/terminal/pull/114)).
- A kitty picture whose requested columns and rows equal its own size is scaled to fill them ([#115](https://github.com/featherbitplatform/terminal/pull/115)).
- Dragging the shown tab onto another tab and dropping it on its content tiles it into that tab again ([#105](https://github.com/featherbitplatform/terminal/pull/105)).

## [0.10.1] - 2026-11-10

### Fixed

- When fbterm closes because the graphics device was lost (the graphics driver stopped responding, or fbterm picked a graphics backend the GPU cannot run), the next launch says so and what to try: update the graphics driver, or on Windows and Linux start fbterm with `WGPU_BACKEND` set to another backend. The crash report gets a matching hint, and the install pages explain the steps ([#99](https://github.com/featherbitplatform/terminal/pull/99)).
- `fbterm.log` now names the graphics adapter, backend and surface format fbterm uses, so a report about a graphics problem carries them ([#99](https://github.com/featherbitplatform/terminal/pull/99)).

## [0.10.0] - 2026-11-09

### Added

- Rename tabs: double-click a tab, choose `Rename tab` from its right-click menu, or press `Cmd+Shift+R` (`Ctrl+Shift+R` on Linux and Windows). `Enter` or a click elsewhere keeps the name, `Escape` cancels, and an empty name brings back the automatic one. The name shows on the tab bar, in the side list and in the window title, and is saved with the layout. The Profiles, Settings, Color schemes and Welcome tabs keep their names ([#96](https://github.com/featherbitplatform/terminal/pull/96)).

### Fixed

- fbterm could quit while the mouse moved over a pane whose session had just restarted, for example while it waited for a password ([#94](https://github.com/featherbitplatform/terminal/pull/94)).
- In the theme gallery, a long `Download` link no longer squeezes the `Use font` button until its label spills out: the buttons move to a new line and the link wraps ([#95](https://github.com/featherbitplatform/terminal/pull/95)).

## [0.9.0] - 2026-11-09

### Added

- The whole app follows your color scheme. The global scheme now colors the tab bar, the sidebar, every page, the panels, dialogs, the palette and menus, not only the terminal; a profile's own scheme still colors only its sessions. Graphite looks as before; other schemes get window colors worked out from their own, with readable text and the scheme's accent ([#91](https://github.com/featherbitplatform/terminal/pull/91)).
- Suggested fonts for themes: every built-in and gallery theme suggests a free font with a `Download` link to its official page and a `Use font` button, and the `Use each theme's suggested font` switch lets the font follow the theme when it is installed. fbterm never downloads fonts ([#88](https://github.com/featherbitplatform/terminal/pull/88)).
- Ligatures: on by default in every font that has them, including the bundled Geist Mono. Turn them off with the `Ligatures` switch, or pick OpenType features with `font_features` in `settings.toml` ([#85](https://github.com/featherbitplatform/terminal/pull/85)).
- Grapheme clusters: programs that turn on mode 2027 get flags, skin tones and joined emoji in one cell. The `grapheme_clusters` setting turns it on by default. Copying text keeps combining accents and whole emoji ([#84](https://github.com/featherbitplatform/terminal/pull/84)).
- Compressed history for very large scrollback: the `history_compression` switch keeps older history compressed, trading some CPU for memory (one tab with a full 1,000,000-line log: about 159 MB instead of 297 MB). `uncompressed_history_lines` sets how many recent lines stay uncompressed. Off by default ([#89](https://github.com/featherbitplatform/terminal/pull/89)).
- macOS: a `Blur style` choice under Background blur. `Native` is the system's blur; `Classic` is fbterm's earlier, clearer blur, where the background opacity drives the look again. Both keep the window's rounded corners ([#87](https://github.com/featherbitplatform/terminal/pull/87)).

### Changed

- Color schemes is a tab, like Settings and Profiles, instead of a separate window. It notices fonts installed while fbterm runs when you switch to it ([#91](https://github.com/featherbitplatform/terminal/pull/91)).
- The Dark/Light `theme` setting is removed: the window follows the color scheme. For a light window choose Graphite Light. An old `theme` line is reported and dropped on the next settings change ([#91](https://github.com/featherbitplatform/terminal/pull/91)).
- Local shells read program output and parse it on separate threads, so busy programs wait less: about 19% faster on plain log output ([#83](https://github.com/featherbitplatform/terminal/pull/83)).

### Fixed

- The Settings font list finds fonts installed while fbterm runs: opening the list looks for them, no restart needed ([#90](https://github.com/featherbitplatform/terminal/pull/90)).
- macOS: tabs opening together, such as restored tabs, no longer fail now and then to start their shell ([#83](https://github.com/featherbitplatform/terminal/pull/83)).

## [0.8.3] - 2026-11-08

### Fixed

- macOS 26 and later: a window with background blur on keeps the same rounded corners as the windows around it. The blur now comes from the system's own blur view, so it looks like standard macOS window translucency ([#80](https://github.com/featherbitplatform/terminal/pull/80)).

## [0.8.2] - 2026-11-08

### Fixed

- macOS 26 and later: windows now have the same rounded corners as the windows around them. The macOS app is built with Xcode 26, which the system needs before it gives an app its current window design; releases since 0.7.0 were built with an older Xcode and kept the smaller corners ([#77](https://github.com/featherbitplatform/terminal/pull/77)).

## [0.8.1] - 2026-11-07

### Fixed

- Row tooltips in the tab column come back after the tabs move to the top while the column's edge is being dragged ([#74](https://github.com/featherbitplatform/terminal/pull/74)).
- A link under the column's edge no longer stays underlined when a drag of the edge ends because the pointer left the window or the window lost focus ([#74](https://github.com/featherbitplatform/terminal/pull/74)).
- With tab groups whose names differ only by case, such as `Prod` and `prod`, a folded group stays folded when the tab that named its header closes ([#74](https://github.com/featherbitplatform/terminal/pull/74)).
- Moving the tabs to the top and back no longer unfolds each window's active group ([#74](https://github.com/featherbitplatform/terminal/pull/74)).
- The tab column measures each tab name once instead of on every redraw ([#74](https://github.com/featherbitplatform/terminal/pull/74)).

## [0.8.0] - 2026-11-07

### Added

- Tab groups in the sidebar: with tabs on the left, the `Group tabs` switch in Settings (or `tab_groups = true` in `settings.toml`) folds the column into the groups your profiles carry. `featherbit` comes first for the Profiles, Settings and Welcome tabs, then the groups by name, then `other`. Click a header to fold or unfold its group; a folded header shows how many tabs it hides, in violet when the selected tab is among them. Tabs reorder only within their group, a new tab opens at the end of its group, and which groups are folded is remembered per window ([#71](https://github.com/featherbitplatform/terminal/pull/71)).
- Resize the tab column: drag its right edge between 140 and 480 pixels. Each window remembers its own width, and a double-click on the edge puts the default back ([#70](https://github.com/featherbitplatform/terminal/pull/70)).
- A tab whose name is cut off in the column shows it whole when you hover it ([#70](https://github.com/featherbitplatform/terminal/pull/70)).

### Changed

- `Cmd+9` (`Ctrl+Shift+9` on Linux and Windows) selects the last tab, however many are open, and is listed as `Last tab` ([#70](https://github.com/featherbitplatform/terminal/pull/70)).
- With tab groups on, next and previous tab and the number shortcuts walk the tabs you can see, skipping folded groups; choosing a tab any other way unfolds its group ([#71](https://github.com/featherbitplatform/terminal/pull/71)).

## [0.7.1] - 2026-11-07

### Added

- A `Copy password` shortcut, `Cmd+Shift+K` (`Ctrl+Shift+K` on Linux and Windows), copying the saved password of the active tab's SSH profile as the tab menu does; also in the palette and in Settings ([#67](https://github.com/featherbitplatform/terminal/pull/67)).
- The tab menu shows an item's shortcut when the key would do the same: on the active tab, beside `Copy password`, and beside `Close tab` when the tab has one pane ([#67](https://github.com/featherbitplatform/terminal/pull/67)).

### Changed

- The `Close pane` action is now `Close tab or pane`, since it closes the tab when the focused pane is its last ([#67](https://github.com/featherbitplatform/terminal/pull/67)).

### Fixed

- The right-click menu's item titles sit on the vertical middle of their row instead of along its top ([#66](https://github.com/featherbitplatform/terminal/pull/66)).

## [0.7.0] - 2026-11-07

With contributions from [AlessandroVisconti](https://github.com/AlessandroVisconti): the Settings and Profiles page tabs, profile icons and the tab controls ([#57](https://github.com/featherbitplatform/terminal/pull/57), [#58](https://github.com/featherbitplatform/terminal/pull/58), [#61](https://github.com/featherbitplatform/terminal/pull/61)).

### Added

- Tabs down the left side of every window: the `tab_bar` setting, the `Vertical tabs` action (`Cmd+Shift+B`, `Ctrl+Shift+B` on Linux and Windows), a `Tabs` row in Settings. The column scrolls, a newly selected tab scrolls into view, and it yields in a window too narrow for it ([#63](https://github.com/featherbitplatform/terminal/pull/63)).
- A right-click menu on a tab, on the bar or in the column: `Copy password` for a password SSH profile's tab, cleared from the clipboard a minute later if still there, and `Close tab` ([#63](https://github.com/featherbitplatform/terminal/pull/63)).
- A Welcome tab opens once at launch and asks the tab layout and whether to check for updates, in place of the update popup. Closing it keeps the defaults ([#63](https://github.com/featherbitplatform/terminal/pull/63)).
- Profile icons: an icon picker in the profile editor; the icon shows on the profile's tabs and in the palette, in the profile's accent color ([#57](https://github.com/featherbitplatform/terminal/pull/57), by AlessandroVisconti).
- Reorder tabs by dragging along the bar, or up and down the column: a marker shows the gap where the tab will land ([#61](https://github.com/featherbitplatform/terminal/pull/61), by AlessandroVisconti).
- Double-clicking a profile in the Profiles tab opens it in a new tab ([#61](https://github.com/featherbitplatform/terminal/pull/61), by AlessandroVisconti).

### Changed

- Settings and Profiles are page tabs in a terminal window instead of separate windows: one of each, opened by their buttons, shortcuts and the palette, closed like a tab, never split or merged. Import and export moved to the Profiles sidebar, and `Edit profiles` is now `Profiles` ([#57](https://github.com/featherbitplatform/terminal/pull/57), [#58](https://github.com/featherbitplatform/terminal/pull/58), by AlessandroVisconti).
- Tab titles show the profile name instead of the path, and tabs shrink to share the bar as it fills ([#61](https://github.com/featherbitplatform/terminal/pull/61), by AlessandroVisconti).
- A wheel click on a tab closes it ([#57](https://github.com/featherbitplatform/terminal/pull/57), by AlessandroVisconti).
- macOS 26: window corners are rounded as the system rounds the windows around them ([#60](https://github.com/featherbitplatform/terminal/pull/60)).
- SSH library updated to russh 0.65.0 for its published advisories, and the docs site's dependencies for theirs ([#59](https://github.com/featherbitplatform/terminal/pull/59)).

### Fixed

- With no profiles saved, `New tab` opens the Profiles tab instead of doing nothing ([#61](https://github.com/featherbitplatform/terminal/pull/61), by AlessandroVisconti).
- Palette actions that need a terminal (find, zoom, maximize, merge) are hidden while a Settings or Profiles tab is shown, where they did nothing ([#61](https://github.com/featherbitplatform/terminal/pull/61), by AlessandroVisconti).
- Disabled switches in Settings no longer react to clicks ([#57](https://github.com/featherbitplatform/terminal/pull/57), by AlessandroVisconti).

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

[0.11.0]: https://github.com/featherbitplatform/terminal/releases/tag/v0.11.0
[0.10.1]: https://github.com/featherbitplatform/terminal/releases/tag/v0.10.1
[0.10.0]: https://github.com/featherbitplatform/terminal/releases/tag/v0.10.0
[0.9.0]: https://github.com/featherbitplatform/terminal/releases/tag/v0.9.0
[0.8.3]: https://github.com/featherbitplatform/terminal/releases/tag/v0.8.3
[0.8.2]: https://github.com/featherbitplatform/terminal/releases/tag/v0.8.2
[0.8.1]: https://github.com/featherbitplatform/terminal/releases/tag/v0.8.1
[0.8.0]: https://github.com/featherbitplatform/terminal/releases/tag/v0.8.0
[0.7.1]: https://github.com/featherbitplatform/terminal/releases/tag/v0.7.1
[0.7.0]: https://github.com/featherbitplatform/terminal/releases/tag/v0.7.0
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
