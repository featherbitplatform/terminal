# Tiling (split panes): design

Status: draft for the owner's review, 2026-10-26 (builder check done). Milestone 2 of
`2026-10-23-featherbit-terminal-design.md` (sections 5.3, 5.5, 5.6, 5.11), which this document
refines. Where they differ, this document wins for tiling.

## 1. Intent

The owner watches several systems at once: logs of different hosts, `top` on another, a harness
being driven. Switching tabs for that is impractical; everything should be visible together in
one tab. The work is mostly watching, with typing in one or two panes.

Success: open one tab, split it into four to six panes on different hosts, size them to taste,
make the noisy log panes smaller, and get exactly that back, reconnected, after quitting and
relaunching the app.

## 2. Goals

1. A tab holds one or more panes laid out by nested splits, side by side or top and bottom, to
   any depth. Each pane is its own session (local shell or any SSH profile, any mix in one tab).
2. Dividers are draggable, so the user decides whether width or height matters more.
3. Each pane has its own zoom (font size step), so a busy pane can show more content in less
   space.
4. Keyboard-first handling, everything also reachable from the palette and rebindable.
5. Panes, split sizes and zoom restore on relaunch, and every pane of the visible tab reconnects
   at once.
6. Search works across panes.
7. Tiles can be rearranged: a tab can be merged into another as a tile, a tile can be moved within
   its tab, and a tile can be turned back into a tab, choosing where it lands each time.

## 3. Non-goals and anti-goals

- **No typing into several panes at once** (no broadcast input). The owner ruled it out.
- **No separate saved layouts** or layout presets: the arrangement simply restores on relaunch,
  like tabs today.
- Not in this version (left for later, not ruled out): moving tabs or tiles between windows.
- Anti-goals, a failure even if tiling works:
  - Tabs without splits must behave exactly as today (one pane, same look, same shortcuts, same
    restore).
  - The redraw contract and pacing (`CLAUDE.md`, gotchas) must hold: no periodic redraws, hidden
    panes never render, output redraws stay paced per window.
  - No host side effects from remote programs, as everywhere in fbterm.
  - Memory: a pane costs what a tab with the same session costs today, no more.

## 4. Behaviour (owner-approved, 2026-10-26)

| What | How |
|---|---|
| Split side by side | Cmd+D (Ctrl+Shift+D elsewhere). The new pane opens the same profile as the focused pane, as a new session (a new shell channel on the same shared connection for SSH). |
| Split top and bottom | Cmd+Shift+D (Ctrl+Shift+Alt+D elsewhere, builder may pick another free chord). |
| Different host in a split | In the palette, a profile has an "Open in split" choice (a modifier on Enter, plus the action listed); plain Enter still opens a new tab. |
| Move focus | Click a pane, or Cmd+Option+arrow (Ctrl+Alt+arrow elsewhere) to the neighbour in that direction. |
| Which pane has focus | Thin violet outline on the focused pane; the others slightly dimmed. With one pane, no outline and no dimming. |
| Resize | Drag a divider; double-click it to share the space equally between its two sides. |
| Zoom | Cmd+plus / Cmd+minus change the focused pane's font size in steps; Cmd+0 resets it to the global size. |
| Maximize | Cmd+Shift+Enter shows the focused pane alone in the tab; again restores the layout. The other panes keep running. |
| Close | Cmd+W closes the focused pane; closing the last pane closes the tab. A pane whose program exits stays open with its notice, as tabs do today. |
| Tab title and color | The tab shows the focused pane's profile name, state and accent. |
| Shortcuts | All of the above are actions in `fbterm-config/src/keys.rs`, shown in the palette and the Settings Keyboard section, rebindable. |

Rearranging *(owner-approved, 2026-10-26)*:

- **Merge a tab into another:** drag a tab from the tab bar onto another tab's content. While
  hovering, a violet preview shows where it will land; dropping makes the dragged tab's pane
  (or all its panes, keeping their arrangement) a tile of the target tab, and the dragged tab
  disappears from the bar.
- **Drop on a tile's edge** (left, right, top, bottom): that tile is split in half and the new
  tile takes the half on that side.
- **Drop on the outer edge of the tab's area:** the new tile takes a whole full-height column or
  full-width row on that side; the existing tiles shrink proportionally to make room.
- **Move a tile within its tab:** pick it up by a small handle that appears when hovering the
  tile (or by dragging with a modifier held; builder's call which, possibly both) and drop it
  with the same two kinds of targets.
- **Tile back to a tab:** drag a tile onto the tab bar; it becomes its own tab at that position.
- **Keyboard:** the palette offers "Merge tab into...", "Move tile..." and "Move tile to new tab",
  with the same choices of target (which tile, which side, or a whole row or column).
- Sessions keep running through every rearrangement: no reconnect, scrollback intact; each moved
  pane gets the new size of its rectangle.

Search:

- Cmd+F finds in the focused pane (the existing find bar, per pane).
- Find in all tabs also searches every pane; each result names its tab and pane, and choosing it
  focuses that pane.
- *(Builder's call, proposed to the owner)* A "Find in this tab" action searches all panes of the
  current tab.

Restore and connections:

- `state.toml` records each tab's split tree, divider ratios, each pane's profile and zoom, and
  the focused pane. An old `state.toml` (tabs only) loads as single-pane tabs.
- On launch every pane of each window's visible tab reconnects immediately (they are all on
  screen); panes of background tabs reconnect when their tab is first shown. This replaces
  "reconnecting only the visible pane" in section 5.3 of the main design.
- Same-profile panes share the profile's one SSH connection (main design 5.3); each pane is its
  own channel. Reconnect keeps a pane's scrollback (`spawn_local_into` / `open_shell_into`).

Settled after the builder check *(my calls, shown to the owner 2026-10-26, who found them right)*:

- **A pane carries everything a tab carries today:** connecting state, the disconnected notice,
  reconnect on Enter, its own find bar.
- **Prompts are per connection, not per pane:** when several panes on one profile connect at
  once, the shared connection is dialed once; the host-key and password prompts appear once, and
  the other panes wait for that connection.
- **Relaunch restores the arrangement, not the sessions' contents:** each pane opens a fresh
  session of its profile, as tabs do today (no scrollback or working directory restored).
- **Export acts on the focused pane.**
- **Background trouble shows on the tab:** if any pane of a tab is disconnected or its program
  has exited, the tab shows a small indicator even when the focused pane is fine.
- **Zoom applies to single-pane tabs too** and is remembered; a tab that is never zoomed looks
  exactly as today.
- **Closing a tab that has more than one live pane asks first** (one confirmation naming the
  number of panes); a single-pane tab closes as today. *(Builder's call; the owner can drop the
  confirmation.)*
- Directional focus goes to the nearest pane whose edge overlaps in that direction (ties: the
  most recently focused). Closing a pane gives its space to its sibling and focuses the nearest
  pane. Dimming is a cheap overlay that changes only on a focus change.

## 5. Constraints

- Everything in the main design and `CLAUDE.md` holds: cross-platform code (macOS releases
  first), Graphite look (violet single accent, no emoji or em-dashes in product copy), no naming
  of other terminal applications, secrets only in the keychain, Conventional Commits with no
  attribution lines.
- Each pane gets its own terminal size (rows and columns) from its rectangle, sent to its PTY or
  SSH channel on every layout change, divider drag or zoom change; resizes during a drag are
  coalesced so a session does not get a resize per mouse move.
- Rendering: one terminal widget per visible pane; only the active tab's panes render, and the
  window's output pacing covers all of them together.
- Memory release (`memory::release_freed()`) also runs after a user closes a pane, exactly as for
  a tab, and never because of anything a remote program does.
- Minimum pane size: a split or drag never makes a pane smaller than a small fixed number of
  columns and rows; the builder picks the numbers.

## 6. Left to the builder

- The data model for the split tree and how it replaces the per-tab single terminal.
- Divider look and hit area within the Graphite design system, the dimming amount, the zoom step
  and limits, minimum pane size.
- The non-macOS default chords where the ones above collide.
- How the per-pane find bar and the find-in-all-tabs results present panes.
- Measurement: add a `dev/perf` scenario with one tab split into several streaming panes, and
  record CPU and memory against today's equivalent number of tabs.
