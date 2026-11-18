# Memo: inline images, so TUIs can show the featherbit logo

Date: 2026-11-14. Status: proposal for the roadmap. Nothing is decided or scheduled.

## Why

featherbittorrent (`../featherbittorrent`) is a ratatui client for fbt-lib. It draws the real featherbit mark (`design-system-v2/assets/featherbit-mark.png`) in its header and on its about screen, but only when the terminal supports an inline image protocol.

- Where no protocol is supported, featherbittorrent shows the text "featherbittorrent" instead.
- That fallback was the owner's decision on 2026-11-14. Half-block and braille renderings were tried and rejected as unreadable at header size.

fbterm currently supports no inline image protocol. Its design (section 5.2) puts sixel and kitty graphics out of scope for now, so inside fbterm featherbittorrent shows only the text title. Supporting inline images would let every featherbit TUI show the mark in our own terminal, and would bring fbterm in line with modern TUIs that use images: previews, plots and logos.

## What featherbittorrent needs

featherbittorrent can use any one of three protocols. In order of preference, fbterm should implement:

1. **The kitty graphics protocol** (APC `ESC _ G … ESC \`): the most capable, and the one TUI libraries target first.
   - Minimum useful subset:
     - transmit PNG data directly (`f=100`, `t=d`), base64 in chunks (`m=1`/`m=0`);
     - display at the cursor (`a=T`), sized in cells (`c=`, `r=`);
     - delete (`a=d`);
     - the query `a=q`, so programs can detect support.
   - Shared memory, file and temp-file transmission (`t=s`, `t=f`, `t=t`) are out of scope. Programs on the remote side of an SSH session must never make fbterm read local files.
2. **Sixel** (DCS `ESC P … q … ESC \`): widely supported by TUI toolkits as a fallback, and works over plain byte streams.
3. **The OSC 1337 `File=` inline image sequence:** simple, and supported by some image libraries.

Detection matters as much as drawing.
- featherbittorrent asks the terminal through the kitty graphics query and the device attributes (sixel shows up in the DA1 reply).
- It treats no answer within a short timeout as "not supported".
- fbterm must answer the query correctly, both when it supports the protocol and when it does not.

## Constraints from fbterm's rules

- **No remote side effects:** inline images are display-only. Image data from a session never touches the clipboard, the file system or anything outside the pane. File-based transmission modes are refused.
- **Bounded memory:** decoding is capped:
  - pixel dimensions and total bytes per image;
  - images per pane, with a total budget that evicts the oldest image when exceeded;
  - chunked transmissions with no end.

  A hostile stream must not grow memory or stall the renderer.
- **Scrollback:** decide whether images scroll with the text and survive in scrollback, or are dropped when they leave the screen. Dropping them is simpler and enough for logos and UIs.
- **Rendering:** images are drawn by the grid renderer from a texture cache, follow damage tracking, and cost nothing in background tabs. This sits on the renderer the spike in section 5.2 validated.
- **Emulation:** sequence parsing belongs in `feathercritty` (our fork), documented against upstream per the fork's merge-ready rule. Upstream does not implement these protocols, so this is a fork-only feature.

## Suggested scope for a first step

1. The kitty graphics protocol, with only direct PNG transmission and display, plus the query answer. Sixel later.
2. Images are placed in cells. They are not kept in scrollback.
3. Memory and size caps with named constants, a fuzz target for the APC parser, and tests that feed hostile sequences.
4. A manual check: featherbittorrent's header and about screen show the mark in fbterm.

## Open questions for the owner

- Should this become a milestone in fbterm's plan, and where in the order relative to the current queue?
- Is the kitty-protocol-only first step enough, or is sixel required from the start?
