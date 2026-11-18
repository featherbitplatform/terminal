---
id: images
title: Inline images
sidebar_label: Images
---

Programs draw pictures in the terminal by writing them into their output as escape sequences. fbterm reads those sequences, decodes the picture and draws it on the grid with everything else, so a picture sits exactly where the program put it, moves with the text when the screen scrolls, and costs no drawing work in a tab you cannot see. A picture is part of the output stream: no file is opened, nothing is written to disk, and a remote program cannot make fbterm touch your machine.

Programs find out what fbterm supports by asking it, so nothing needs configuring. A picture appears when a program sends one.

## What works

| Protocol | What it gives you here |
| --- | --- |
| **kitty graphics** (`ESC _ G ... ESC \`) | PNG, RGB and RGBA data, sent whole or in chunks, optionally zlib compressed. Placements by cell span (`c` columns by `r` rows), a source rectangle inside the picture, pixel offsets within the starting cell, and a z-index that puts a picture under or over the text. Image ids and image numbers, re-placing a stored picture without sending it again, deletion with the protocol's selectors, the support query, and Unicode placeholders: the private-use `U+10EEEE` cells carrying diacritics and a color that some programs print instead of asking for a placement. |
| **sixel** (`ESC P ... q ... ESC \`) | The DEC standard format, decoded to pixels by fbterm itself: raster attributes, the full color register model with 1024 registers per picture (RGB and HLS definitions, the factory default palette in registers 0 to 15), repeat runs, graphics carriage return and newline, transparent backgrounds. |
| **OSC 1337 inline images** (`ESC ] 1337 ; File=... : <base64> BEL`) | `inline=1` pictures with `width` and `height` given in cells, pixels, percent or `auto`, and `preserveAspectRatio`. |

Transmission is display only. The file-based kitty modes (`t=f`, `t=t`) and shared memory (`t=s`) are refused with the protocol's error reply, so a program never makes fbterm read a local file or fetch anything. An OSC 1337 sequence that asks for a download without display has its payload read and dropped.

This works in every kind of tab:

- A local shell on macOS or Linux.
- An SSH session: the picture arrives as part of the output stream, so it follows the same rules there as locally.
- A local shell on Windows, through the ConPTY console layer fbterm ships with. See [Windows](#windows).

fbterm answers the queries programs use to work this out: the device attributes reply carries the graphics bit (`CSI ? 62;4;22c`), the graphics attributes query reports 1024 sixel color registers and the text area in pixels, and the kitty support query answers OK for what works and with the protocol's own error for what does not. The cell size and text area reports (`CSI 14 t`, `CSI 16 t`) answer in physical pixels at the window's display scale, which is what image programs size their output by: a program that sizes a picture to N cells gets exactly N cells, at any font size and on any display.

## Settings

| Setting | In the Settings tab | Default | What it does |
| --- | --- | --- | --- |
| `inline_images` | **Inline images** under Terminal, with the line `Show pictures that programs send` | `true` | Show pictures programs send. Off, every picture sequence a program writes is consumed and discarded, what is already on screen disappears at once, and programs are told graphics are not supported from then on. Turning it back on does not bring the pictures back. Applies to open tabs at once. |
| `scrollback_images` | **Images in scrollback** under Terminal, with the line `Keep pictures when they scroll off the screen` | `false` | Keep a picture when its rows have all scrolled off the top of the screen: on, it stays in history with its lines and scrolls back into view; off, it is replaced by an [`[image W×H]`](#image-placeholders) line and its memory is freed. Only counts while `inline_images` is on, so that row is dimmed and its switch unusable while it is off. Applies to open tabs at once. |

```toml
# Show pictures that programs send (true or false). Off, every picture a program writes is
# dropped, and what is already on screen disappears with it.
#inline_images = true

# Keep a picture when its rows scroll off the screen (true or false): off replaces it with a
# line saying how big it was. Only counts while inline_images is true.
#scrollback_images = false
```

Both are ordinary keys in [`settings.toml`](files#settingstoml). The file fbterm creates writes them exactly as above, as comments with their defaults; remove the leading `#` to set one by hand.

## Image placeholders

When a picture cannot be drawn any more, its first row shows a dim line naming its pixel size, for example `[image 640×480]`. It appears in two situations:

- **Scrolled away,** with `scrollback_images = false` (the default): the picture's rows have all left the screen, so the line replaces it and its memory is freed. With `scrollback_images = true` the picture itself stays in history instead and no line is drawn.
- **Out of memory:** a [memory cap](#limits) took the picture's pixels while its placement is still on screen, so the line shows in its place until the program sends that picture again.

The line is drawn over the row in the pane's text color at 55% opacity and is never written into the grid or into history. It is not selectable, copying does not include it, and find does not match it: it is a hint about what was there, not content. Pictures shown through kitty Unicode placeholders get no line of their own, because the cells they cover are already text.

## Limits

A program (or a hostile stream) cannot make pictures cost more than this. Every number is fixed in fbterm.

| Limit | Value | What happens when it is reached |
| --- | --- | --- |
| One picture's size | 10 000 × 10 000 pixels, and 64 MiB decoded | The picture is not shown: a kitty client gets the protocol's error (`ENOMEM` or `EINVAL`), a sixel is drawn as far as it fits and the rest is ignored, an OSC 1337 picture is dropped without a reply. |
| One transmission | 16 MiB of payload | That transmission is dropped: a kitty client gets `ENOMEM`, an OSC 1337 picture is discarded without a reply. A chunked kitty transmission that stops making progress for 10 seconds is dropped the same way. |
| One tab | 256 MiB of decoded picture data, resampled copies included | That tab's oldest scrollback pictures become `[image W×H]` lines first; when none are left to reclaim, the least recently shown buffers go, and any of them that was on screen shows its placeholder line. |
| The whole app | 512 MiB of decoded picture data, resampled copies included | Same order across all tabs: scrollback pictures first, then the least recently shown buffers, whichever tab they belong to. |
| Pictures in one tab | 4 096 placements on screen (and as many placeholder-cell placements), 512 stored pictures kept for re-placing, 100 000 `[image W×H]` records | The oldest are dropped and free their memory. |

Nothing else is affected when a limit is reached: text keeps working, other tabs keep their pictures, and a tab you cannot see does no drawing work at all (no resampling, no upload), although it still parses and decodes within the caps because a program's reply depends on the decode result.

Corrupt picture data is refused rather than shown, and it never takes anything down: a kitty client gets `EBADPNG`, sixel and OSC 1337 stay silent, and the rest of the stream keeps going.

## Image formats

The decoders accept PNG, JPEG and GIF (first frame), and raw RGB and RGBA pixels from the kitty protocol; fbterm produces the pixels itself for sixel. Palette, gray-scale and 16-bit PNGs are converted to 8-bit RGBA on the way in.

Animation is not supported yet: an animated GIF or an APNG shows its first frame, and the kitty animation actions are answered with the protocol's "not supported" error. Animation is planned as a later feature.

## Using it

Any program that speaks one of these protocols draws in a tab exactly as the protocol describes. To try it with a file:

```sh
kitten icat image.png
img2sixel image.png
chafa -f sixel image.png
```

`kitten icat` sends the picture through the kitty graphics protocol, `img2sixel` and `chafa` send it as sixel. Run them in a tab and the picture appears where the cursor was. The same commands work in an SSH tab when the program runs on the far side: what travels is the same output stream.

Claude Code shows its pictures here once you start it with `CLAUDE_CODE_FORCE_TERMINAL_IMAGES=1` set in the environment it inherits, for example `export CLAUDE_CODE_FORCE_TERMINAL_IMAGES=1` before `claude`.

## Security

Pictures are display only, and that is a hard rule rather than a default:

- A program on the far side of SSH never makes fbterm read local files, shared memory or temporary files, write anything to disk, touch the clipboard, or start anything. The kitty file modes (`t=f`, `t=t`) and shared memory (`t=s`) are refused with `EBADF`; an OSC 1337 download that does not ask for display is discarded.
- Nothing about a picture is kept on disk: no cache, no temporary file, nothing left behind after the tab closes. Decoders read the bytes from memory.
- The [caps](#limits) bound what one stream can cost, so an endless chunked transmission, an enormous sixel or an unterminated sequence cannot grow memory without limit or stall drawing.

## Windows

Local shells on Windows talk through a console layer (ConPTY) that sits between the program and fbterm, and which layer that is decides whether pictures arrive at all: Microsoft's system one drops the kitty APC and sixel DCS sequences a program writes, so those pictures never reach fbterm. fbterm therefore installs Microsoft's open-source ConPTY (`conpty.dll` and `OpenConsole.exe`, MIT licensed, with its license and third-party notices) beside `fbterm.exe` from the `.msi`; local shells then run under that copy instead. See [Install on Windows](install-windows). SSH tabs are not involved: their output reaches fbterm directly.

Which console layer a run is using is written at the start of `fbterm.log` (see [Where files live](files)):

```text
local shells use the bundled ConPTY
```

If the installed copy is missing, the line says so and what it means:

```text
conpty.dll not found beside fbterm.exe: local shells use the system ConPTY, which drops kitty and sixel images
```
