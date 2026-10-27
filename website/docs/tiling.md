---
id: tiling
title: Split panes
sidebar_label: Split panes
---

A tab can hold several panes, side by side or one above the other, to any depth. Each pane is its own session: a local shell or any SSH profile, in any mix. Use it to watch the logs of two hosts, a build and a shell in one place, without switching tabs.

![A tab split into three panes: two side by side above a third](/img/tiling-split.png)

A tab with one pane looks and behaves as it always did: no outline, no dimming, no handle. The focused pane of a split tab has a thin accent outline and the others are slightly dimmed. The tab shows the focused pane's profile name and accent, and a small indicator when any other pane of the tab is disconnected or its program has exited.

A pane whose profile has an accent color also gets a thin border in that accent while the tab shows more than one pane, so hosts stay recognizable side by side; the focused pane's outline then uses its accent. Each pane also uses its own profile's [color scheme](color-schemes).

## Splitting and moving around

| Action | macOS | Linux and Windows |
| --- | --- | --- |
| Split right | `Cmd+D` | `Ctrl+Shift+D` |
| Split down | `Cmd+Shift+D` | `Ctrl+Shift+Alt+D` |
| Close pane | `Cmd+W` | `Ctrl+Shift+W` |
| Focus the pane to the left, right, above, below | `Cmd+Option+Left`, `Right`, `Up`, `Down` | `Ctrl+Alt+Left`, `Right`, `Up`, `Down` |

Splitting opens the focused pane's profile in a new pane and moves focus there. To open a different profile in a split, select it in the palette and press `Shift+Enter` instead of `Enter`; the palette also lists **Split right** and **Split down**. A split is refused, silently, when a pane would end up smaller than 20 columns by 5 rows.

Click a pane to focus it. By direction, focus goes to the nearest pane whose edge overlaps in that direction; when two are equally near, the one you used more recently. Closing a pane gives its space to the pane next to it. A pane whose program exits stays open with its notice, like a tab.

Drag a divider to resize; double-click it to share the space equally between its two sides. Nothing you type goes to more than one pane.

## Zoom

| Action | macOS | Linux and Windows |
| --- | --- | --- |
| Zoom in | `Cmd+=` | `Ctrl+Shift+=` |
| Zoom out | `Cmd+-` | `Ctrl+Shift+-` |
| Reset zoom | `Cmd+0` | `Ctrl+Shift+0` |

Zoom changes the text size of the focused pane only, so a noisy log pane can show more in less space. It is remembered with the layout. Reset returns the pane to the font size in Settings.

## Maximize and restore

`Cmd+Shift+Enter` (`Ctrl+Shift+Enter` on Linux and Windows) shows the focused pane alone in the tab; press it again to restore the layout. **Maximize pane** in the palette does the same. The other panes keep running out of sight, and moving focus or splitting shows them again. It does nothing in a tab with one pane.

## Moving tiles

Sessions keep running through every rearrangement: no reconnect, and the scrollback stays. Each moved pane takes the size of its new rectangle.

- **Move a tile within its tab.** Hover a pane (in a tab with more than one) to show a small grip centred on its top edge, and drag it. Holding `Cmd` (`Ctrl+Shift` on Linux and Windows) and dragging from anywhere in the pane works too; a click with the key held, without moving, still reaches the program, so links and mouse-aware programs keep working. Drop on a tile's edge (left, right, top or bottom) to split that tile and put the moved one in the half on that side, or within the outer edge of the tab's area to give it a whole column or row on that side, with the others shrinking to make room. A preview shows where it will land, and a drop where a pane would be too small does nothing. `Escape` cancels.
- **Move a tile to a tab of its own.** Drag the tile onto the tab bar: a bar marks where the new tab will go. In the palette, **Move tile to new tab** puts it at the end of the bar.
- **Merge a tab into another.** Drag a tab from the tab bar onto another tab's content: the dragged tab's pane, or all its panes in their arrangement, becomes tiles of that tab and the dragged tab leaves the bar. Dropping works like moving a tile, on a tile's edge or on the outer edge. Holding the drag over another tab in the bar for half a second shows that tab, so you can aim at its content.
- **From the palette.** **Merge tab into…** asks for the tab, then for where: next to a pane (left, right, above, below) or as a whole column or row on a side. **Move tile…** asks the same for the focused pane within its tab. Neither is bound by default; give them a shortcut under **Keyboard** in Settings (`merge_tab`, `move_tile`, `tile_to_tab` in the [`[keys]` table](keyboard-shortcuts#the-keys-table)).

## Search

`Cmd+F` (`Ctrl+Shift+F`) finds in the focused pane, with its own find bar. **Find in all tabs** also searches every pane: each result names its tab and pane, and choosing one focuses that pane. **Find in this tab** in the palette opens the same box limited to the panes of the current tab. See [Search](keyboard-shortcuts#search-in-detail).

## Closing

Closing a pane closes its session without asking; the last pane closes the tab. The tab's close button on a tab with more than one live pane asks first: **Close this tab?** with the text "Its 3 panes and their sessions will close.", and the buttons **Close tab** and **Cancel**. A tab with one pane closes at once.

## Restore

The layout is saved with your windows: each tab's arrangement, the divider positions, each pane's profile and zoom, and which pane has focus. On launch every pane of the visible tab of each window reconnects at once, since they are all on screen; the panes of other tabs reconnect when you first show their tab. Each pane opens a fresh session of its profile (no scrollback or working directory comes back), and panes on the same SSH profile share its one connection, so a password or host-key prompt appears once. A saved state from before split panes loads as tabs with one pane each.
