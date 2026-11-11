---
id: tabs
title: Tabs
sidebar_label: Tabs
---

Every open session is a tab. A tab shows its profile's name (or the [name you gave it](#renaming-a-tab)), a dot for the connection state (connecting, connected, disconnected) and the profile's accent as a stripe or as a tint over the whole tab (**Color as** in the profile editor). A tab with [split panes](tiling) shows the focused pane's name and a second dot when another pane of the tab is disconnected or its program has exited.

## The tab bar

Tabs sit along the top of the window, with the buttons for a new tab, Profiles, tunnels, the file panel and Settings after them. Click a tab to show it, its close button or a wheel click to close it, and drag a tab left or right to reorder it. Dragging a tab onto another tab's terminal merges it as a pane there; dragging a pane's handle onto the bar makes it a tab of its own (see [Moving tiles](tiling#moving-tiles)).

## Tabs on the left

With many sessions open, a row of tabs runs out of room. Choose **Tabs: Left** in Settings (Appearance) or press `Cmd+Shift+B` (`Ctrl+Shift+B` on Linux and Windows) to list the tabs down the left side of every window instead. The palette offers the same as **Vertical tabs**, and `tab_bar = "left"` in [`settings.toml`](files#settingstoml) does the same by hand; `"top"` puts them back.

![Tabs listed down the left side of the window, with the terminal beside them](/img/tabs-left.png)

Everything a tab does works in the column: select, close, reorder by dragging up and down, drop onto a tab's terminal to merge, drop a pane's handle between rows to make it a tab. The column scrolls when the tabs overflow, and the tab you select (by click or shortcut) scrolls into view. The bar keeps its buttons. In a window too narrow for the column and a 200 pixel terminal, the column gives way so the terminal keeps its width.

Drag the column's right edge to make it narrower or wider, from 140 to 480 pixels; each window remembers its own width, and a double-click on the edge puts the default back. A tab whose name is cut off at the edge shows it whole when you hover it. `Cmd+9` (`Ctrl+Shift+9`) selects the last tab, however many are open.

## Tab groups

With tabs on the left, turn on **Group tabs** in Settings (Appearance), or set `tab_groups = true` in [`settings.toml`](files#settingstoml), and the column folds into the groups your profiles carry (the `group` field in [`profiles.toml`](profiles), edited in the Profiles page). `featherbit` comes first, for the Profiles, Settings and Welcome tabs; then the groups by name; then `other`, for tabs without one. A header shows only while one of its tabs is open.

Click a header to fold or unfold its group. A folded header shows how many tabs it hides, in violet when the selected tab is among them. Tabs can be dragged only within their group, and a new tab opens at the end of its group. Next and previous tab and the number shortcuts walk the tabs you can see, skipping folded groups; choosing a tab any other way (the palette, a double-click in the Profiles page) unfolds its group. Which groups are folded is remembered per window. With tabs on top the setting does nothing.

## Renaming a tab

Double-click a tab, on the bar or in the column, to give it a name of its own. You can also choose **Rename tab** from the [tab menu](#the-tab-menu), or press `Cmd+Shift+R` (`Ctrl+Shift+R` on Linux and Windows) for the active tab. The palette lists it as **Rename tab** too.

A field takes the place of the tab's name, with the current name selected. Press `Enter` or click anywhere else to keep what you typed, or `Escape` to keep the old name. While the field is open, keys go to it and never to the terminal, and shortcuts wait until it closes.

The name replaces the profile's name on the bar, and the program's title in the column and in the window title. It belongs to the tab, so it stays when you move focus between the tab's panes. It is saved with the layout and comes back when fbterm restarts. To go back to the automatic name, rename the tab and leave the field empty.

The Profiles, Settings, Color schemes and Welcome tabs cannot be renamed. Only you can rename a tab: nothing a program in a terminal sends changes the name.

## When a session ends

When an SSH connection drops or fails, or a local program exits, the pane keeps its text and scrollback and shows a bar along its bottom saying why: `Disconnected (connection reset)`, `Could not connect: …` or `Process exited with code 1`. The bar has a **Reconnect** button for an SSH profile and **Restart** for a local one, with the shortcut beside it. The same line is written, dimmed, into the terminal, so the scrollback keeps a record of where the session ended.

There are three ways back, and each continues in the same pane, below the old output:

- Press `Enter` in the pane.
- Click **Reconnect** (or **Restart**) on the bar. It brings back that pane only.
- Press `Cmd+Shift+O` (`Ctrl+Shift+O` on Linux and Windows), or choose **Reconnect** from the [tab menu](#the-tab-menu). Both bring back every ended pane of the tab at once, so a split tab whose panes all lost the same server comes back in one step. Panes that are still connected are left alone. The palette lists it as **Reconnect** too.

The tab's **With the shell** [tunnels](tunnels) start again once the shell is back. Nothing a program in a terminal sends can reconnect or restart a session.

## The tab menu

Right-click a tab, on the bar or in the column, for a small menu:

- **Reconnect** brings back every pane of the tab whose session ended; see [When a session ends](#when-a-session-ends). It appears only when one did, and reads **Restart** when only local programs exited.
- **Rename tab** opens the field described in [Renaming a tab](#renaming-a-tab). It is not offered for the Profiles, Settings, Color schemes and Welcome tabs.
- **Copy password** puts the saved password of the tab's SSH profile on the clipboard. It appears only for an SSH profile that logs in with a password, and copies only what is saved: nothing is asked for, and nothing is read from the running session. See [Copying a password](saved-secrets#copying-a-password) for what happens when no password is saved, when the vault is locked, and how the clipboard is cleared afterwards.
- **Close tab** does what the tab's close button does, asking first when the tab has several live panes.

Each item shows its shortcut when the key would do the same thing: the menu of the active tab shows `Cmd+Shift+R` (`Ctrl+Shift+R`) beside **Rename tab**, `Cmd+Shift+K` (`Ctrl+Shift+K`) beside **Copy password**, `Cmd+Shift+O` (`Ctrl+Shift+O`) beside **Reconnect** or **Restart**, and `Cmd+W` (`Ctrl+Shift+W`) beside **Close tab** when the tab has one pane (with several, the key closes only the focused pane). The menu of another tab shows no shortcuts, because the keys act on the active tab.

A right-click never selects the tab. Only your own right-click opens the menu: nothing a program in a terminal sends can.

## The Welcome tab

The first time fbterm starts without `tab_bar` or `check_updates` in `settings.toml` (a fresh install, or the first launch after upgrading to a version with these settings), a **Welcome** tab opens beside your sessions and asks two things: where open sessions should be listed, with a small picture of each layout, and whether fbterm may [check for updates](files#update-check). Each choice applies and is saved as soon as you click it, so you can see the tabs move before deciding. **Done**, the tab's close button or the close-tab shortcut closes it; whatever you did not answer keeps its default (tabs on top, no update check), and the tab does not come back. Quitting with it still open leaves the questions open, so it shows again next time.

![The Welcome tab: a picture of each tab layout to choose from, and the update check question](/img/welcome.png)
