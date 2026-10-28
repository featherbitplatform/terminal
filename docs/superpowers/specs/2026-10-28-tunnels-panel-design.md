# Tunnels panel: design

Status: draft for the owner's review, 2026-10-28. Changes where tunnels are shown; everything
else about tunnels stays as `2026-10-27-tunnels-design.md` specifies (and that document wins on
behaviour). The file panel's design (`2026-10-27-file-panel-design.md`) is the model for the
panel's placement.

## 1. Intent

The owner wants tunnels next to the terminal, not in a separate window: open them like the file
panel, see every tunnel at once, and have the tunnels of the host being worked on stand out.

Success: the owner is in a tab of profile `prod`, presses the tab bar's tunnels button, and a
panel opens on the right edge of that window listing every tunnel of every profile, with
`prod`'s group at the top, highlighted and expanded. Switching to a tab of `staging` moves the
highlight to `staging`. Opening the file panel closes the tunnels panel and the other way round.

## 2. Goals

1. A **tunnels panel** docked at the right edge of a terminal window, like the file panel,
   showing what the Tunnels window shows today: every tunnel of every SSH profile, grouped by
   profile, each with its addresses, state dot, on/off switch and Retry when it failed, under
   the search bar (same fuzzy filter by tunnel and profile name). Groups are always expanded
   (there is no collapsing, as today).
2. **Following the active tab:** when the window's focused pane belongs to a profile with
   tunnels, that profile's group is highlighted: listed first and marked with the accent. It
   moves with that window's focus (tab switch, pane focus); focusing another window (editor,
   Settings) changes nothing. A local pane
   or a profile without tunnels highlights nothing and the order is the normal one.
3. **One panel at a time on the right edge:** opening the tunnels panel closes that window's
   file panel and opening the file panel closes the tunnels panel. (Owner's decision.)
4. The **Tunnels window goes away.** Everything that opened it opens or closes the panel in the
   window it was used from: the tab bar's tunnels button, the `Tunnels` action and its shortcut,
   the palette's `Tunnels` row. Used from a window that has no dock (profile editor, Settings,
   Color schemes, export and import), the action brings the most recently focused terminal
   window forward and opens the panel there. The palette's per-tunnel rows keep switching tunnels on and off
   without opening anything.

## 3. Non-goals and anti-goals

- No change to how tunnels run, connect, retry or ask questions (host key, password still ask
  over the window the switch was used in, as today).
- No editing of tunnel definitions in the panel (they stay in the profile editor).
- Anti-goals:
  - A remote program opening, closing or changing the panel or any tunnel.
  - Losing the search text, the scroll position or a tunnel's state because the highlight
    moved or the panel was reopened.
  - The panel stealing keyboard input from the terminal when it is not focused (the file
    panel's keyboard rules apply).

## 4. Behaviour

- **Per window**, like the file panel: each terminal window has its own panel open or closed.
  Its width is shared and remembered across restarts (`state.toml`, like `file_panel_width`;
  a separate `tunnels_panel_width`). Whether it is open is not restored at launch, matching the
  file panel.
- **Placement and look** follow the file panel: same edge, same resize handle and width limits,
  same header with a title (`Tunnels`) and close button, Graphite tokens. The rows look as in
  the Tunnels window today.
- **The tab bar's tunnels button** keeps its indicator (count of tunnels up) and shows the
  pressed/active style while the panel is open in that window.
- **Search and keyboard:** each window's panel keeps its own search text for the app session,
  also while closed; the search field takes focus when the panel opens. Escape in the panel
  clears the search, else gives the keyboard back to the terminal (as in the file panel); the
  panel closes with its close button, the tab bar button or the `Tunnels` shortcut. Cmd+W /
  Ctrl+Shift+W keep closing the tab. Closing a window forgets its panel state.
- **Highlight:** the focused pane's profile group is first; its header uses the accent; nothing
  else changes order (the other groups keep the window's current order). If the search hides
  the highlighted group, nothing is highlighted until it shows again.
- **Empty states:** no SSH profile has tunnels: the same message the window shows today, with
  where to add tunnels (the profile editor).
- **Debug aid:** `FBTERM_OPEN_TUNNELS=1` opens the panel in the first window instead of the
  window, so screenshots keep working; `FBTERM_SCREENSHOT_WINDOW=tunnels` goes away.

## 5. Decided by the owner (2026-10-28)

- A panel like the file panel instead of the Tunnels window.
- It lists every tunnel of every profile; the active tab's profile is highlighted.
- Opening one of the two right-edge panels closes the other.

## 6. Calls made without the owner (check these)

- Highlight = listed first, accent on the group header; other groups keep their order (the
  profile order); groups never collapse.
- From a window without a dock, the `Tunnels` action raises the most recently focused terminal
  window and opens the panel there.
- Search text per window, kept while the panel is closed, forgotten with the window; focus on
  open; Escape clears the search, then gives the keyboard back; Cmd+W still closes the tab.
- Width remembered, open/closed not restored at launch (as the file panel); an earlier guess in
  the conversation said "remembers whether it was open", corrected here to match the file panel.
- The tunnels button shows an active style while its panel is open.
- `FBTERM_SCREENSHOT_WINDOW=tunnels` is removed; `FBTERM_OPEN_TUNNELS=1` opens the panel.

## 7. Left to the builder

How the panel shares code with the file panel's dock (one dock with two contents is fine), the
exact accent treatment, keyboard focus handling inside the panel, and tests. Docs: the website's
tunnels page (its "The Tunnels window" section and screenshot), keyboard shortcuts, CLAUDE.md.
