# A scrollable tab bar with buttons that stay in view

Status: implemented on `feature/UI-tabs-improvements`; the strip is a custom clipped widget with the offset kept in `App` instead of an iced scrollable, and the active tab is never left partly out of the strip: a scroll by hand passes over it or shows it whole, and the chevrons show only with room for them and a whole minimum tab; narrower, the strip takes all the room and the tabs shrink down to 50 px (no close button below 80 px), and the drag room is 10 px, never wider than the strip (see the plan). Owner's request, 2026-11-10: with many tabs or a narrow window the
bar's trailing buttons (new tab, Profiles, Tunnels, Files, Settings) run off the right edge,
under the drawn window controls or out of the window. They must always be visible; the tabs
scroll instead. Everything marked "builder's call" is open to change by the builder; the rest
is the owner's decision.

## Why

`tab_bar` (`window.rs`) puts the tabs and the buttons in one row. Tabs shrink from 196 px to a
minimum and stop there, so past that point the row is wider than the window and its tail, the
buttons, is cut off or hidden behind the window controls on Windows and Linux. The buttons are
the way into Profiles, Tunnels and Settings, so losing them with a few dozen tabs open is a
real failure.

## Behavior (owner's decisions)

- **Tab width.** Tabs share the room as today, from 196 px down to **80 px** (was 90,
  `TAB_MIN_WIDTH`). Only when 80 px tabs still do not fit does the strip scroll.
- **The button group.** `+`, Profiles, Tunnels, Files (while shown) and Settings stay one group,
  in today's order. The group starts right after the last tab, as now, and moves right as tabs
  are added until it reaches the end of the free space. That end is the window's right edge
  (less today's 8 px padding), or the inner edge of the drawn window controls on Windows and
  Linux (either side they sit on). macOS has no controls, so it uses the full width: the space
  is computed from what is there, never assumed. Once at that end the group stays put.
- **The strip.** The tabs live in a strip between the left inset and the group (or the
  chevrons, below). When their natural width fits, the strip is exactly that wide and nothing
  scrolls. When it does not, the strip takes all the room left of the chevrons and the group,
  less 10 px of empty bar (`DRAG_ROOM`, reserved in every layout) to drag the window by, and
  scrolls sideways.
- **Scrolling.** The mouse wheel or trackpad over the strip scrolls it sideways (a vertical
  wheel too, since the bar has no vertical axis), and so do two chevron buttons.
- **Chevrons.** Lucide `chevron-left` and `chevron-right`, placed together between the strip and
  the button group, same button style as the group's (ghost, 14 px icon, 7 px padding). A chevron
  acts only while tabs are hidden on its side. A click moves the strip by one tab (a tab's width
  plus the 2 px spacing) in its direction, clamped to the ends. They exist only while the strip
  scrolls.
- **Tooltips.** None required for the chevrons.
- **The active tab is always found.** Selecting a tab by click, shortcut, palette or
  tab-switching, opening one, or resizing the window so the active tab no longer fits scrolls
  just enough to show it fully (the same rule the left-hand column uses). A tab the user scrolls
  away from by hand stays where it was left until the active tab changes or the window resizes.
- **Left-hand tabs** (`tab_bar = "left"`) are unchanged: the column already scrolls, and gets no
  chevrons. With tabs on the left the bar holds only the buttons, which already fit.
- **Everything a tab does keeps working in the strip:** select, close, middle-click close,
  double-click rename, right-click menu, drag to reorder, drop a tile or tab as a new tab,
  merge by dropping on a terminal, the carried tab's violet gap marker.

## Goals, non-goals, anti-goals

Goals: buttons always in view and never under the window controls; the strip scrolls with wheel
and chevrons; no behavior of a tab is lost.

Non-goals: a scrollbar on the bar; an overflow dropdown listing hidden tabs; kinetic or animated
scrolling beyond what iced's scrollable gives; any change to the left-hand column, tab groups,
the tab menu or tab drag semantics.

Anti-goals (a failure even if it technically works): the buttons ever overlapping the window
controls or leaving the window; a periodic subscription or redraw loop while idle (CLAUDE.md's
redraw contract: scrolling, drag edge-scrolling and reveals are driven by input events only);
the content outline, the active tab's corner and flare pixels or the translucent bar painting
twice or off whole physical pixels (all existing rules in CLAUDE.md "Gotchas" stay true); a
changed look when everything fits (the common case must render exactly as today, tab width
aside).

## Where it meets the code

Read these first; the design leans on them.

- `window.rs`: `tab_bar`, `tab_width`, `trailing_width`, `TabBar`, `BarBackground` (its `layout`
  and `tab_rects` treat the tabs as the first children of the bar's row, and `update`/`draw`
  use those rectangles for drags, the hole, flares, hover tints and the gap marker). The strip
  changes where the tabs live, so these readers must find them in the strip and shift by its
  scroll offset.
- `tab_drag.rs`: `Strip` already has a `scroll` field ("how far the strip's coordinates are
  shifted from the window's"), used by the sidebar, whose rows live inside a scrollable and are
  measured in content coordinates. The bar passes 0 today and will pass its own offset.
- `tab_sidebar.rs`: the pattern to mirror. A scrollable with an id (`scroll_id`), `on_scroll`
  sending `TabBarMsg::Scrolled { offset, height }`, `App::sidebar_scroll` remembering
  `(offset, viewport)`, `reveal(top, height, offset, viewport)` (pure, tested) computing the
  scroll that shows an item, and `App::reveal_active_tabs` (after every update) issuing
  `scroll_to` when the active tab changed. The bar needs the horizontal twin of each, in a
  `TabBarMsg` variant of its own so the two axes never share state. `reveal_active_tabs` today
  returns early and forgets its state when tabs are on top; that branch becomes the bar's reveal.
- `ui.rs` and `dev/subset-icons.sh`: `ICON_RIGHT` (chevron-right) exists; `chevron-left` is not
  in the subset font. Add it to `ICONS`, regenerate `crates/fbterm/assets/fonts/lucide-icons.ttf`
  (needs npm and `pyftsubset`, both present on the owner's machine) and add `ICON_LEFT`.
- `chrome.rs`: `leading_inset`, `controls_width`, `setup().controls.side`; the drawn controls'
  plate sits beside the bar, so the bar's own width already excludes it. `tab_width` today
  subtracts the controls itself from the window width; keep one source of truth for the free
  room.

## How

1. **Layout is computed, not discovered.** One pure function takes the window width (`None`:
   full size, as `tab_width` does), chrome, tab count and trailing width and returns the tab
   width, the strip's width and whether it overflows. Tab width is `clamp(room / n - 2, 80,
   196)` as today with the new floor, the room already less `DRAG_ROOM`; overflow is when `n * (tab_width + 2)` exceeds the room
   left after the group; the strip's width is the natural width or, on overflow, the room less the
   group and the chevrons. Everything downstream (`tab_bar`, tests) uses it. It replaces the
   width logic of `tab_width`.
2. **The strip is a horizontal scrollable** with no scrollbar, like the sidebar's list, so the
   wheel, clamping and `scroll_to` come from iced. If iced's horizontal scrollable does not turn a
   vertical wheel into sideways motion, map it (a small wrapper or in `BarBackground::update`,
   which already sees every bar event). Verify this before relying on it.
3. **Chevron space is reserved** while the strip overflows, and the chevron with nothing hidden
   on its side is drawn dimmed and ignores presses, so the strip does not change width, and the
   tabs do not jump, when you scroll to an end. This is the builder's call over hiding it
   outright; keep it if it feels right, change it if it does not.
4. **`BarBackground` reads the strip.** The bar's tab rectangles come from the strip's content,
   shifted by the offset into window coordinates (the same relation `Strip::scroll` states); the
   drag protocol runs with the real offset. Hover tints, the hole, flares and the gap marker
   are clipped to the strip's visible bounds. The active tab partly out of view: its hole and
   flares are cut at the strip's edge (no flare, and no outline arc, on a clipped side). The
   active tab fully out of view: no hole, and the outline runs unbroken along the content's
   top. The reveal rule makes both states transient after a selection, so keep this simple, but
   it must not draw outside the strip or panic with an empty or off-screen rectangle.
5. **Auto-scroll while dragging** a tab or a tile near either end of the strip (within about
   24 px) so it can be dropped further along: owner's "yes", mechanism the builder's. It must be
   driven by pointer-move events only (no timer), each move scrolling as far as the pointer travelled (at most 24 px), so the speed does not follow the device's event rate; the cost is that a perfectly still pointer at
   the edge does not keep scrolling, which is accepted.
6. **State and messages.** A `TabBarMsg` variant for the strip's scroll (offset, visible width)
   next to the sidebar's, an `App` map keyed by window (cleared with the window like
   `sidebar_scroll`), one for the chevrons, reveal through the shared `reveal` function (rename
   its parameters from top/height to start/length if it reads better; keep the sidebar's tests
   green).
7. **Chrome variants.** Check `Unified` (macOS: traffic-light inset on the left, nothing on the
   right), `Drawn` with controls left and right, `Native` (Linux, no drawn controls), full
   screen (`leading_inset` is 0), and a docked panel (the bar spans the window regardless; the
   outline ends at the panel's rule).
8. **Debug aid, builder's call.** `FBTERM_OPEN_TABS` with many profiles plus `FBTERM_SCREENSHOT`
   already shows a crowded bar; a way to open the first window at a narrow width, or the demo
   tab count, would help. Add only what the screenshots need, listed in CLAUDE.md's debug section.

## Tests

- The pure layout function: everything fits (strip equals natural width, no overflow, tab width
  as before above 80 px); exact overflow threshold; the floor at 80 px; each chrome (no
  controls, controls reducing the room, full screen); the Files button and tunnel count widening
  the group (the existing `the_trailing_room_follows_the_files_button_and_the_tunnel_count` and
  `tabs_shrink_with_the_room_and_stop_at_the_minimum` are updated to 80 px).
- The reveal calculation for the horizontal axis (the shared pure function covers it; add cases
  for a tab left of, right of and half-hidden by the strip).
- `BarBackground` with a scrolled strip: a click, a drag threshold and a drop land on the tab
  under the pointer in window coordinates (extend the harness in `window.rs`' tests that drives
  the bar with events); the hole and flares for an active tab clipped by the strip, fully
  hidden, and fully visible (extend `the_outline_goes_around_the_active_tab_on_whole_pixels`
  style tests; whole-physical-pixel checks at fractional scales).
- App level: `reveal_active_tabs` scrolls the strip for a newly active tab only when tabs are on
  top and it is not fully visible, and leaves the sidebar's behaviour untouched.
- A screenshot of the bar with enough tabs to overflow, on macOS and with Windows' demo chrome
  (`FBTERM_DEMO_CHROME=windows`), looked at through `FBTERM_SCREENSHOT` per CLAUDE.md.

## Docs to update when built

- `website/docs/tabs.md`: the bar scrolls when tabs do not fit; the chevrons and the wheel; the
  buttons stay in view. A screenshot of the overflowing bar if the build makes one.
- `CLAUDE.md`: the tab-bar gotcha paragraph (where the strip, its reveal and its message live,
  and that the bar's tab rectangles are strip content shifted by the offset), the debug-aid list
  if one is added, and the status paragraph's line on the tab bar. The changelog is written by
  the release branch, not here.
- `dev/subset-icons.sh`'s `ICONS` and the regenerated font for `chevron-left`.
