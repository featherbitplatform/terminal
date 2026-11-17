//! Wrapper around the `feathercritty` emulator grid.

use std::sync::Arc;

use fbterm_config::Rgb;
use feathercritty::event::WindowSize;
use feathercritty::event::{Event, EventListener};
use feathercritty::grid::{Dimensions, Scroll};
use feathercritty::term::{Config, HistoryDone, HistoryJob, Term};
use feathercritty::vte::ansi::{Processor, Rgb as VteRgb};
use parking_lot::Mutex;

use crate::Selection;
use feathercritty::index::Side;
use feathercritty::selection::{Selection as EmuSelection, SelectionType};

pub use feathercritty::event::{ImageData, ImageKey};
pub use feathercritty::term::TermMode;

/// Terminal size in cells plus the pixel size of one cell.
///
/// `cell_width` and `cell_height` are LOGICAL pixels, exactly as the widget measures and draws
/// them; `scale` is the window's physical scale (physical pixels per logical one, 1.0 when the
/// window has not reported one). Programs - PTYs, SSH, the emulator's own reports - see physical
/// pixels, which [`Self::physical_cell`] and [`Self::physical_area`] give.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridSize {
    pub cols: u16,
    pub rows: u16,
    pub cell_width: f32,
    pub cell_height: f32,
    /// Physical pixels per logical pixel of the window this grid was measured in.
    pub scale: f32,
}

impl GridSize {
    /// One text cell in physical pixels: each axis is the logical cell rounded to a whole pixel at
    /// `scale`, and at least 1 (what the emulator places images by and reports for a cell).
    pub fn physical_cell(&self) -> (u32, u32) {
        (
            whole(self.cell_width * self.scale).max(1),
            whole(self.cell_height * self.scale).max(1),
        )
    }

    /// The whole grid in physical pixels: each axis is its cells rounded to a whole pixel at
    /// `scale`, so a program's idea of the drawable area matches what is drawn. An empty axis is
    /// 0. Saturates instead of overflowing.
    pub fn physical_area(&self) -> (u32, u32) {
        (
            whole(f32::from(self.cols) * self.cell_width * self.scale),
            whole(f32::from(self.rows) * self.cell_height * self.scale),
        )
    }
}

/// Logical pixels at a window's scale, as whole physical pixels: rounded, and saturating to
/// `u32::MAX` rather than overflowing (`NaN` becomes 0).
fn whole(logical: f32) -> u32 {
    logical.round().max(0.0) as u32
}

/// Events the terminal raises towards the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TermEvent {
    Title(String),
    Bell,
    /// Replies (device attributes, status reports, ...) that must be written back to the session.
    PtyWrite(Vec<u8>),
    Exit,
    /// A picture a program sent: the emulator parsed it, the host decodes (if needed) and draws
    /// it. `key` names it from then on, and an `Encoded` payload is answered once the host
    /// reports its decode result to the emulator.
    ImageReceived {
        key: ImageKey,
        data: ImageData,
    },
    /// Nothing references this picture any more, so the host frees its copy. Raised exactly once
    /// per key, when its last placement, placeholder or stored reference is gone.
    ImageReleased(ImageKey),
}

struct Shared {
    events: Vec<TermEvent>,
    size: GridSize,
    foreground: Rgb,
    background: Rgb,
    cursor: Rgb,
    ansi: [Rgb; 16],
}

impl Shared {
    fn color(&self, index: usize) -> Rgb {
        match index {
            0..=15 => self.ansi[index],
            16..=231 => {
                let i = (index - 16) as u8;
                let lv = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
                Rgb {
                    r: lv(i / 36),
                    g: lv(i / 6 % 6),
                    b: lv(i % 6),
                }
            }
            232..=255 => {
                let v = 8 + (index - 232) as u8 * 10;
                Rgb { r: v, g: v, b: v }
            }
            257 => self.background,
            258 => self.cursor,
            _ => self.foreground,
        }
    }
}

const fn rgb(r: u8, g: u8, b: u8) -> Rgb {
    Rgb { r, g, b }
}

const XTERM_ANSI: [Rgb; 16] = [
    rgb(0, 0, 0),
    rgb(205, 0, 0),
    rgb(0, 205, 0),
    rgb(205, 205, 0),
    rgb(0, 0, 238),
    rgb(205, 0, 205),
    rgb(0, 205, 205),
    rgb(229, 229, 229),
    rgb(127, 127, 127),
    rgb(255, 0, 0),
    rgb(0, 255, 0),
    rgb(255, 255, 0),
    rgb(92, 92, 255),
    rgb(255, 0, 255),
    rgb(0, 255, 255),
    rgb(255, 255, 255),
];

/// Collects emulator events into a shared queue and answers emulator queries.
#[derive(Clone)]
pub struct EventProxy {
    shared: Arc<Mutex<Shared>>,
}

impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        let mut sh = self.shared.lock();
        let mapped = match event {
            Event::Title(t) => TermEvent::Title(t),
            Event::Bell => TermEvent::Bell,
            Event::PtyWrite(s) => TermEvent::PtyWrite(s.into_bytes()),
            // Programs never write the clipboard: OSC 52 stores are dropped.
            Event::ClipboardStore(..) => return,
            Event::Exit | Event::ChildExit(_) => TermEvent::Exit,
            Event::TextAreaSizeRequest(fmt) => {
                let s = sh.size;
                let (w, h) = s.physical_cell();
                TermEvent::PtyWrite(
                    fmt(WindowSize {
                        num_lines: s.rows,
                        num_cols: s.cols,
                        cell_width: w.min(u16::MAX as u32) as u16,
                        cell_height: h.min(u16::MAX as u32) as u16,
                    })
                    .into_bytes(),
                )
            }
            Event::ColorRequest(index, fmt) => {
                let c = sh.color(index);
                TermEvent::PtyWrite(
                    fmt(VteRgb {
                        r: c.r,
                        g: c.g,
                        b: c.b,
                    })
                    .into_bytes(),
                )
            }
            // Never read the clipboard on behalf of the program: reply with an empty payload.
            Event::ClipboardLoad(_, fmt) => TermEvent::PtyWrite(fmt("").into_bytes()),
            // Pictures are the host's to decode and draw; releases tell it when to let go.
            Event::ImageReceived { key, data } => TermEvent::ImageReceived { key, data },
            Event::ImageReleased(key) => TermEvent::ImageReleased(key),
            _ => return,
        };
        sh.events.push(mapped);
    }
}

struct Dims {
    cols: usize,
    rows: usize,
}

impl Dimensions for Dims {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

fn dims(size: GridSize) -> Dims {
    Dims {
        cols: usize::from(size.cols.max(1)),
        rows: usize::from(size.rows.max(1)),
    }
}

/// Ids handed to [`crate::images`] for decoded-image accounting, one per terminal ever made.
static NEXT_IMAGE_OWNER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// An emulated terminal: grid, parser and pending events.
pub struct Terminal {
    pub(crate) term: Term<EventProxy>,
    config: Config,
    parser: Processor,
    shared: Arc<Mutex<Shared>>,
    /// This terminal's id in the app-wide image store ([`Terminal::image_owner`]).
    image_owner: u64,
    /// The `inline_images` setting: whether pictures programs send are shown at all. The emulator
    /// starts with image support on, like the default here.
    images_enabled: bool,
    /// The `scrollback_images` setting: whether a picture stays when its rows have all scrolled
    /// off the screen. Off, like the default, it becomes a placeholder line and is released.
    scrollback_images: bool,
    pub(crate) generation: u64,
    /// Calls of [`Terminal::drop_history_cache`] (tests: the read cache itself is not visible
    /// outside feathercritty).
    #[cfg(test)]
    pub(crate) cache_drops: usize,
}

pub type SharedTerminal = Arc<Mutex<Terminal>>;

impl Terminal {
    pub fn new(size: GridSize, scrollback: usize) -> Self {
        let shared = Arc::new(Mutex::new(Shared {
            events: Vec::new(),
            size,
            foreground: rgb(229, 229, 229),
            background: rgb(0, 0, 0),
            cursor: rgb(229, 229, 229),
            ansi: XTERM_ANSI,
        }));
        let proxy = EventProxy {
            shared: shared.clone(),
        };
        let config = Config {
            scrolling_history: scrollback,
            osc52: feathercritty::term::Osc52::OnlyPaste,
            ..Config::default()
        };
        let mut term = Term::new(config.clone(), &dims(size), proxy);
        // The emulator places images and answers `CSI 14 t` / `CSI 16 t` from the cell's physical
        // size, so it gets it before any program can ask (see [`Self::set_cell_pixel_size`]).
        let (cell_w, cell_h) = size.physical_cell();
        term.set_cell_pixel_size(cell_w, cell_h);
        Self {
            term,
            config,
            parser: Processor::new(),
            shared,
            image_owner: NEXT_IMAGE_OWNER.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            images_enabled: true,
            scrollback_images: false,
            generation: 0,
            #[cfg(test)]
            cache_drops: 0,
        }
    }

    /// Feed bytes received from the session into the emulator.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
        self.generation += 1;
    }

    /// This terminal's id in the app-wide image store ([`crate::images::store`]). The store
    /// accounts decoded pixels per owner so one tab cannot crowd the others out, and frees a
    /// tab's whole budget when this terminal drops.
    pub fn image_owner(&self) -> u64 {
        self.image_owner
    }

    /// Report the result of decoding an image the store took from [`crate::images::store`]'s
    /// queue: `Ok` lets the emulator answer the program's transmission `OK`, the errors answer
    /// `EBADPNG` and `ENOMEM`. The store calls this from its worker through a registered
    /// [`crate::images::OwnerHooks`]; callers must `take_events` afterwards to deliver the reply.
    pub fn image_decoded(
        &mut self,
        key: feathercritty::event::ImageKey,
        result: Result<(), feathercritty::event::ImageDecodeError>,
    ) {
        self.term.image_decoded(key, result);
        self.generation += 1;
    }

    /// Run the graphics protocols' stall watchdog (see `Term::tick`): drops a stalled chunked
    /// kitty transmission and delivers image events parked by a resize. Call after feeding bytes
    /// so replies arrive even while the terminal is otherwise idle.
    pub fn tick(&mut self, now: std::time::Instant) {
        self.term.tick(now);
        self.generation += 1;
    }

    /// Turn the oldest scrollback image into a placeholder line and release it outright, so a
    /// session over its share of the image store can reclaim memory until enough went (`None`
    /// when scrollback holds no image). The emulator sends the matching `ImageReleased`.
    pub fn evict_oldest_scrollback_image(&mut self) -> Option<feathercritty::event::ImageKey> {
        let key = self.term.evict_oldest_scrollback_image();
        if key.is_some() {
            self.generation += 1;
        }
        key
    }

    /// Accept the kitty keyboard protocol from programs (off by default). While off, programs'
    /// requests to push, pop, set or query enhancement flags are ignored, so keys keep their
    /// legacy encoding. Switching it either way drops any flags a program has set.
    pub fn set_kitty_keyboard(&mut self, enabled: bool) {
        if self.config.kitty_keyboard == enabled {
            return;
        }
        self.config.kitty_keyboard = enabled;
        let seen = self.shared.lock().events.len();
        self.term.set_options(self.config.clone());
        self.drop_reannounced_title(seen);
        self.generation += 1;
    }

    /// Set the scrollback limit of the normal screen (the alternate screen keeps none). Raising
    /// it keeps everything; lowering it drops the oldest lines at once, and a view scrolled
    /// further back is pulled down to the new oldest line. Returns how many history lines were
    /// dropped. Like [`Terminal::set_kitty_keyboard`], the title the emulator re-announces is
    /// discarded.
    pub fn set_scrollback(&mut self, lines: usize) -> usize {
        let before = self.history_len();
        self.config.scrolling_history = lines;
        let seen = self.shared.lock().events.len();
        self.term.set_options(self.config.clone());
        self.drop_reannounced_title(seen);
        self.generation += 1;
        before.saturating_sub(self.history_len())
    }

    /// Compress history lines older than the `keep` newest (`None`: compress nothing, and
    /// decompress what is compressed). Only marks blocks: the work is done by history jobs,
    /// which [`crate::history_worker`] runs off the lock. Like [`Terminal::set_kitty_keyboard`],
    /// the title the emulator re-announces is discarded.
    pub fn set_history_compression(&mut self, keep: Option<usize>) {
        if self.config.history_compression == keep {
            return;
        }
        self.config.history_compression = keep;
        let seen = self.shared.lock().events.len();
        self.term.set_options(self.config.clone());
        self.drop_reannounced_title(seen);
        self.generation += 1;
    }

    /// The history compression setting (`None`: off).
    pub fn history_compression(&self) -> Option<usize> {
        self.config.history_compression
    }

    /// Whether history blocks may be waiting to be compressed or decompressed. O(1), so it is
    /// checked after every feed.
    pub fn has_history_jobs(&self) -> bool {
        self.term.has_history_jobs()
    }

    /// The next history job, with a copy of its block; run it without the lock.
    pub fn next_history_job(&mut self) -> Option<HistoryJob> {
        self.term.next_history_job()
    }

    /// Up to `max` history jobs, oldest first; run them without the lock.
    pub fn next_history_jobs(&mut self, max: usize) -> Vec<HistoryJob> {
        self.term.next_history_jobs(max)
    }

    /// Install a finished history job. `false` when the result is stale (the history changed
    /// since the job was taken) and was dropped; that is normal.
    pub fn install_history_job(&mut self, done: HistoryDone) -> bool {
        self.term.install_history_job(done)
    }

    /// Free the decompressed history blocks kept for reads (up to 256 KiB with compression on;
    /// nothing otherwise). Called once after a sweep of the whole history (a search, an export),
    /// so the blocks it read last do not stay resident.
    pub fn drop_history_cache(&mut self) {
        self.term.drop_history_cache();
        #[cfg(test)]
        {
            self.cache_drops += 1;
        }
    }

    /// Run and install every pending history job now, under the caller's lock (tests and
    /// measurements; the app leaves them to [`crate::history_worker`]).
    pub fn run_history_jobs(&mut self) {
        self.term.run_history_jobs();
    }

    /// Lines of scrollback the normal screen holds (while the alternate screen is up, the
    /// normal screen's, which is kept aside).
    pub fn history_len(&self) -> usize {
        if self.term.mode().contains(TermMode::ALT_SCREEN) {
            self.term.inactive_grid().history_size()
        } else {
            self.term.grid().history_size()
        }
    }

    /// Setting options re-announces the current title; nothing changed, so remove that one event
    /// (raised after `seen` events were pending) and keep any title a program set before.
    fn drop_reannounced_title(&self, seen: usize) {
        let mut sh = self.shared.lock();
        if let Some(i) =
            (seen..sh.events.len()).find(|&i| matches!(sh.events[i], TermEvent::Title(_)))
        {
            sh.events.remove(i);
        }
    }

    pub fn kitty_keyboard(&self) -> bool {
        self.config.kitty_keyboard
    }

    /// Show pictures programs send (`enabled`), and whether a picture whose rows have all scrolled
    /// off the screen stays in the history or becomes a line saying how big it was
    /// (`keep_scrollback`).
    ///
    /// Switching `enabled` off drops everything the emulator already stored or displayed: each
    /// picture on screen gets one [`TermEvent::ImageReleased`] and its cells are freed; programs'
    /// picture sequences are consumed and answered as unsupported from then on. Switching it back
    /// on does not bring the pictures back. Unlike [`Terminal::set_kitty_keyboard`], nothing is
    /// re-announced here, so no title event needs dropping.
    pub fn set_images(&mut self, enabled: bool, keep_scrollback: bool) {
        if self.images_enabled != enabled {
            self.images_enabled = enabled;
            self.term.set_images_enabled(enabled);
            self.generation += 1;
        }
        if self.scrollback_images != keep_scrollback {
            self.scrollback_images = keep_scrollback;
            self.term.set_scrollback_images(keep_scrollback);
            self.generation += 1;
        }
    }

    /// Whether pictures programs send are shown (the `inline_images` setting).
    pub fn images_enabled(&self) -> bool {
        self.images_enabled
    }

    /// Whether pictures stay when their rows scroll off the screen (the `scrollback_images`
    /// setting).
    pub fn scrollback_images(&self) -> bool {
        self.scrollback_images
    }

    /// Start with grapheme clusters on (DEC private mode 2027) after a reset or a restart. This
    /// only sets the default: the mode a running program has is never flipped. Like
    /// [`Terminal::set_kitty_keyboard`], the title the emulator re-announces is discarded.
    pub fn set_grapheme_clusters(&mut self, on: bool) {
        if self.config.grapheme_clusters == on {
            return;
        }
        self.config.grapheme_clusters = on;
        let seen = self.shared.lock().events.len();
        self.term.set_options(self.config.clone());
        self.drop_reannounced_title(seen);
        self.generation += 1;
    }

    /// The grapheme clusters default (the setting), not the live mode.
    pub fn grapheme_clusters(&self) -> bool {
        self.config.grapheme_clusters
    }

    /// Put mode 2027 back to the default, for a new session taking over this terminal.
    pub fn reset_grapheme_clusters(&mut self) {
        self.term.reset_grapheme_cluster_mode();
        self.generation += 1;
    }

    /// Counter bumped by every change that can alter what is drawn.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The current text selection, in grid coordinates.
    ///
    /// Stored as the emulator's own selection, which moves with its text when output scrolls and
    /// is dropped when that text is erased or scrolls out of history.
    pub fn selection(&self) -> Option<Selection> {
        let range = self.term.selection.as_ref()?.to_range(&self.term)?;
        let mut sel = Selection::start(range.start);
        sel.update(range.end);
        Some(sel)
    }

    pub fn set_selection(&mut self, selection: Option<Selection>) {
        self.term.selection = selection.map(|s| {
            let (start, end) = s.bounds();
            let mut inner = EmuSelection::new(SelectionType::Simple, start, Side::Left);
            inner.update(end, Side::Right);
            inner
        });
        self.generation += 1;
    }

    /// Show the oldest line of the scrollback.
    pub fn scroll_to_top(&mut self) {
        self.term.scroll_display(Scroll::Top);
        self.generation += 1;
    }

    /// Show the newest output (leave scrollback).
    pub fn scroll_to_bottom(&mut self) {
        self.term.scroll_display(Scroll::Bottom);
        self.generation += 1;
    }

    /// Resize the grid, and with it the cell size programs are told: the new grid carries the
    /// window's physical scale (see [`Self::set_cell_pixel_size`]).
    pub fn resize(&mut self, size: GridSize) {
        self.shared.lock().size = size;
        let (cell_w, cell_h) = size.physical_cell();
        self.term.resize(dims(size));
        self.term.set_cell_pixel_size(cell_w, cell_h);
        self.generation += 1;
    }

    /// The physical pixel size of one text cell. Placements convert pixels to cells with it, and
    /// `CSI 14 t` / `CSI 16 t` answer from it, so programs that size their output by the cell see
    /// what is drawn. Each axis is clamped to at least 1 by the emulator.
    pub fn set_cell_pixel_size(&mut self, width: u32, height: u32) {
        self.term.set_cell_pixel_size(width, height);
        self.generation += 1;
    }

    /// Set the palette used to answer colour queries.
    pub fn set_colors(&mut self, foreground: Rgb, background: Rgb, cursor: Rgb, ansi: [Rgb; 16]) {
        let mut sh = self.shared.lock();
        sh.foreground = foreground;
        sh.background = background;
        sh.cursor = cursor;
        sh.ansi = ansi;
        drop(sh);
        self.generation += 1;
    }

    pub fn size(&self) -> GridSize {
        self.shared.lock().size
    }

    /// Scroll the viewport; positive moves towards older history.
    pub fn scroll(&mut self, delta_lines: i32) {
        self.term.scroll_display(Scroll::Delta(delta_lines));
        self.generation += 1;
    }

    pub fn mode(&self) -> TermMode {
        *self.term.mode()
    }

    pub fn take_events(&mut self) -> Vec<TermEvent> {
        std::mem::take(&mut self.shared.lock().events)
    }

    pub fn term(&self) -> &Term<EventProxy> {
        &self.term
    }
}

impl Drop for Terminal {
    /// Hand this terminal's id back to the image store: queued work for it is dropped and its
    /// share of the app-wide budget is freed (the store holds no strong reference back, so the
    /// tab dropping is what ends the terminal → hooks → store chain).
    fn drop(&mut self) {
        crate::images::store().drop_owner(self.image_owner);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use feathercritty::index::{Column, Line};
    use feathercritty::term::cell::Flags;
    use feathercritty::vte::ansi::{Color, Rgb};

    fn size(cols: u16, rows: u16) -> GridSize {
        GridSize {
            cols,
            rows,
            cell_width: 8.0,
            cell_height: 16.0,
            scale: 1.0,
        }
    }

    /// A grid of fractional cells (logical px) in a window at `scale`.
    fn grid_of(cols: u16, rows: u16, cell_width: f32, cell_height: f32, scale: f32) -> GridSize {
        GridSize {
            cols,
            rows,
            cell_width,
            cell_height,
            scale,
        }
    }

    /// A fractional advance (7.4) and a ceiled line height (15.6): what `measure` gives a font
    /// whose size is not a whole multiple of 5.
    #[test]
    fn physical_cell_is_the_logical_cell_rounded_at_the_scale() {
        for (scale, w, h) in [(1.0, 7, 16), (2.0, 15, 31), (1.25, 9, 20), (1.5, 11, 23)] {
            let g = grid_of(80, 24, 7.4, 15.6, scale);
            assert_eq!(g.physical_cell(), (w, h), "scale {scale}");
        }
    }

    #[test]
    fn physical_cell_is_at_least_one_pixel() {
        let tiny = grid_of(80, 24, 0.2, 0.4, 1.0);
        assert_eq!(tiny.physical_cell(), (1, 1));
        // A window that reported no scale at all still has cells of a whole pixel.
        let unscaled = grid_of(80, 24, 7.4, 15.6, 0.0);
        assert_eq!(unscaled.physical_cell(), (1, 1));
    }

    #[test]
    fn physical_area_is_the_grid_rounded_at_the_scale() {
        let g = grid_of(100, 30, 8.4, 16.2, 2.0);
        // 100 x 8.4 x 2 = 1680, 30 x 16.2 x 2 = 972: the whole grid, not one cell times cells.
        assert_eq!(g.physical_area(), (1680, 972));
        let at_one = grid_of(100, 30, 8.4, 16.2, 1.0);
        assert_eq!(at_one.physical_area(), (840, 486));
    }

    #[test]
    fn physical_area_of_an_empty_axis_is_zero() {
        let g = grid_of(0, 0, 8.0, 16.0, 2.0);
        assert_eq!(g.physical_area(), (0, 0));
        assert_eq!(
            g.physical_cell(),
            (16, 32),
            "the cell is still a whole pixel"
        );
        let rows_only = grid_of(40, 0, 8.0, 16.0, 1.0);
        assert_eq!(rows_only.physical_area(), (320, 0));
    }

    #[test]
    fn physical_sizes_saturate_instead_of_overflowing() {
        let huge = grid_of(u16::MAX, u16::MAX, 1e20, 1e20, 1e9);
        assert_eq!(huge.physical_cell(), (u32::MAX, u32::MAX));
        assert_eq!(huge.physical_area(), (u32::MAX, u32::MAX));
        // A product that is not even a finite number.
        let infinite = grid_of(10, 10, f32::MAX, f32::MAX, f32::MAX);
        assert_eq!(infinite.physical_cell(), (u32::MAX, u32::MAX));
        assert_eq!(infinite.physical_area(), (u32::MAX, u32::MAX));
    }

    #[test]
    fn feed_plain_text_lands_in_grid() {
        let mut t = Terminal::new(size(80, 24), 100);
        t.feed(b"hi");
        let g = t.term().grid();
        assert_eq!(g.screen()[Line(0)][Column(0)].c, 'h');
        assert_eq!(g.screen()[Line(0)][Column(1)].c, 'i');
    }

    #[test]
    fn sgr_truecolor_sets_cell_fg() {
        let mut t = Terminal::new(size(80, 24), 100);
        t.feed(b"\x1b[38;2;10;20;30mX");
        let c = &t.term().grid().screen()[Line(0)][Column(0)];
        assert_eq!(
            c.fg,
            Color::Spec(Rgb {
                r: 10,
                g: 20,
                b: 30
            })
        );
    }

    #[test]
    fn wide_char_occupies_two_cells() {
        let mut t = Terminal::new(size(80, 24), 100);
        t.feed("界".as_bytes());
        let g = t.term().grid();
        assert!(
            g.screen()[Line(0)][Column(0)]
                .flags
                .contains(Flags::WIDE_CHAR)
        );
        assert!(
            g.screen()[Line(0)][Column(1)]
                .flags
                .contains(Flags::WIDE_CHAR_SPACER)
        );
    }

    #[test]
    fn resize_reflows_and_keeps_scrollback() {
        let mut t = Terminal::new(size(80, 24), 1000);
        for i in 0..100 {
            t.feed(format!("line {i}\r\n").as_bytes());
        }
        t.resize(size(40, 24));
        assert!(t.term().grid().history_size() > 0);
        assert_eq!(t.term().grid().columns(), 40);
    }

    #[test]
    fn scrollback_is_capped() {
        let mut t = Terminal::new(size(80, 24), 100);
        for i in 0..1000 {
            t.feed(format!("line {i}\r\n").as_bytes());
        }
        assert!(t.term().grid().history_size() <= 100);
        assert!(t.term().grid().history_size() > 0);
    }

    fn fill(t: &mut Terminal, n: usize) {
        for i in 0..n {
            t.feed(format!("line {i}\r\n").as_bytes());
        }
    }

    #[test]
    fn set_scrollback_raises_and_lowers_limit() {
        let mut t = Terminal::new(size(20, 5), 100);
        fill(&mut t, 500);
        assert_eq!(t.history_len(), 100);
        assert_eq!(t.set_scrollback(300), 0, "raising drops nothing");
        fill(&mut t, 500);
        assert_eq!(t.history_len(), 300);
        t.set_scrollback(50);
        assert_eq!(t.history_len(), 50);
        fill(&mut t, 500);
        assert_eq!(t.history_len(), 50, "the new limit holds as output arrives");
    }

    #[test]
    fn set_scrollback_keeps_alt_screen_at_zero() {
        let mut t = Terminal::new(size(20, 5), 100);
        fill(&mut t, 50);
        let held = t.history_len();
        t.feed(b"\x1b[?1049h");
        assert_eq!(t.term().grid().history_size(), 0);
        let dropped = t.set_scrollback(20);
        assert_eq!(
            dropped,
            held - 20,
            "the kept-aside normal screen is trimmed"
        );
        fill(&mut t, 100);
        assert_eq!(
            t.term().grid().history_size(),
            0,
            "alt screen has no history"
        );
        t.feed(b"\x1b[?1049l");
        assert_eq!(t.history_len(), 20);
    }

    #[test]
    fn set_scrollback_keeps_title() {
        let mut t = Terminal::new(size(20, 5), 100);
        t.feed(b"\x1b]2;my title\x07");
        assert_eq!(t.take_events(), vec![TermEvent::Title("my title".into())]);
        t.set_scrollback(10);
        assert!(t.take_events().is_empty(), "no title re-announced");
        t.feed(b"\x1b]2;next\x07");
        t.set_scrollback(20);
        assert_eq!(t.take_events(), vec![TermEvent::Title("next".into())]);
    }

    #[test]
    fn set_scrollback_reports_dropped_lines() {
        let mut t = Terminal::new(size(20, 5), 1000);
        fill(&mut t, 200);
        let held = t.history_len();
        assert!(held > 100);
        assert_eq!(t.set_scrollback(40), held - 40);
        assert_eq!(t.set_scrollback(40), 0);
        assert_eq!(t.set_scrollback(0), 40);
        assert_eq!(t.history_len(), 0);
    }

    #[test]
    fn lowering_scrollback_clamps_scrolled_view() {
        let mut t = Terminal::new(size(20, 5), 5000);
        fill(&mut t, 5100);
        assert_eq!(t.history_len(), 5000);
        t.scroll_to_top();
        assert_eq!(t.term().grid().display_offset(), 5000);
        let old_top = t.stable_line(feathercritty::index::Line(-5000));
        let old_kept = t.stable_line(feathercritty::index::Line(-1000));
        assert!(t.grid_line(old_top).is_some());
        assert_eq!(t.set_scrollback(1000), 4000);
        assert!(t.term().grid().display_offset() <= 1000);
        assert_eq!(t.grid_line(old_top), None, "dropped lines resolve to None");
        assert!(t.grid_line(old_kept).is_some());
        t.scroll(5);
        t.scroll_to_bottom();
    }

    #[test]
    fn device_attributes_query_produces_ptywrite() {
        let mut t = Terminal::new(size(80, 24), 100);
        t.feed(b"\x1b[c");
        let ev = t.take_events();
        assert!(
            ev.iter()
                .any(|e| matches!(e, TermEvent::PtyWrite(b) if !b.is_empty()))
        );
        assert!(t.take_events().is_empty());
    }

    fn reply(t: &mut Terminal) -> String {
        let b: Vec<u8> = t
            .take_events()
            .into_iter()
            .filter_map(|e| {
                if let TermEvent::PtyWrite(b) = e {
                    Some(b)
                } else {
                    None
                }
            })
            .flatten()
            .collect();
        String::from_utf8(b).unwrap()
    }

    #[test]
    fn osc11_background_query_gets_reply() {
        let mut t = Terminal::new(size(80, 24), 100);
        t.feed(b"\x1b]11;?\x07");
        assert!(reply(&mut t).contains("rgb:"));
    }

    #[test]
    fn csi_14t_gets_pixel_size_reply() {
        let mut t = Terminal::new(size(80, 24), 100);
        t.feed(b"\x1b[14t");
        assert_eq!(reply(&mut t), "\x1b[4;384;640t");
    }

    /// The emulator answers `CSI 16 t` (one cell) and `CSI 14 t` (the text area) from the physical
    /// cell size the host reported with `set_cell_pixel_size`, so a program that sizes its output
    /// by the cell uses the numbers the drawing actually uses.
    #[test]
    fn size_reports_answer_in_physical_pixels() {
        let mut t = Terminal::new(grid_of(10, 5, 8.4, 16.2, 2.0), 100);
        t.feed(b"\x1b[16t");
        assert_eq!(
            reply(&mut t),
            "\x1b[6;32;17t",
            "the cell at the window's scale"
        );
        t.feed(b"\x1b[14t");
        // 10 cells of 17 px, 5 rows of 32.
        assert_eq!(reply(&mut t), "\x1b[4;160;170t");

        // Resizing carries the new grid's scale too.
        t.resize(grid_of(20, 8, 8.4, 16.2, 1.0));
        t.feed(b"\x1b[16t\x1b[14t");
        assert_eq!(reply(&mut t), "\x1b[6;16;8t\x1b[4;128;160t");
    }

    #[test]
    fn osc52_read_gets_empty_reply() {
        let mut t = Terminal::new(size(80, 24), 100);
        t.feed(b"\x1b]52;c;?\x07");
        assert_eq!(reply(&mut t), "\x1b]52;c;\x07");
    }

    #[test]
    fn osc52_write_is_ignored() {
        let mut t = Terminal::new(size(80, 24), 100);
        t.feed(b"\x1b]52;c;aGVsbG8=\x07");
        assert!(t.take_events().is_empty());
    }

    fn select_word_and_scroll(prefill: usize) -> (String, String) {
        use feathercritty::index::Point;
        let mut t = Terminal::new(size(20, 5), 100);
        for i in 0..prefill {
            t.feed(format!("old {i}\r\n").as_bytes());
        }
        t.feed(b"\x1b[2J\x1b[Hhello world\r\n");
        let mut sel = Selection::start(Point::new(Line(0), Column(6)));
        sel.update(Point::new(Line(0), Column(10)));
        t.set_selection(Some(sel));
        let before = t.selection().unwrap().text(&t);
        for i in 0..8 {
            t.feed(format!("new {i}\r\n").as_bytes());
        }
        let after = t.selection().map(|s| s.text(&t)).unwrap_or_default();
        (before, after)
    }

    #[test]
    fn selection_follows_text_when_output_scrolls() {
        assert_eq!(select_word_and_scroll(0), ("world".into(), "world".into()));
        // Full history: lines drop off the top while history size stays constant.
        assert_eq!(
            select_word_and_scroll(500),
            ("world".into(), "world".into())
        );
    }

    #[test]
    fn generation_tracks_changes() {
        let mut t = Terminal::new(size(80, 24), 100);
        let g0 = t.generation();
        t.feed(b"x");
        let g1 = t.generation();
        assert!(g1 > g0);
        t.set_selection(None);
        assert!(t.generation() > g1);
    }

    #[test]
    fn scroll_to_bottom_leaves_history() {
        let mut t = Terminal::new(size(80, 24), 1000);
        for i in 0..100 {
            t.feed(format!("line {i}\r\n").as_bytes());
        }
        t.scroll(10);
        assert_eq!(t.term().grid().display_offset(), 10);
        t.scroll_to_bottom();
        assert_eq!(t.term().grid().display_offset(), 0);
    }

    fn esc(t: &Terminal) -> Vec<u8> {
        use iced::keyboard::key::{Code, Named, Physical};
        use iced::keyboard::{Key, Modifiers};
        crate::encode_key(
            &Key::Named(Named::Escape),
            Physical::Code(Code::Escape),
            None,
            Modifiers::empty(),
            t.mode(),
        )
        .unwrap()
    }

    #[test]
    fn grapheme_setting_sets_default_not_live_mode() {
        let mut t = Terminal::new(size(80, 24), 100);
        assert!(!t.grapheme_clusters());
        t.set_grapheme_clusters(true);
        assert!(t.grapheme_clusters());
        assert!(
            !t.mode().contains(TermMode::GRAPHEME_CLUSTER),
            "never flips a live mode"
        );
        t.reset_grapheme_clusters();
        assert!(t.mode().contains(TermMode::GRAPHEME_CLUSTER));
    }

    /// `(char, wide)` for the first `n` cells of line 0.
    fn cells(t: &Terminal, n: usize) -> Vec<(char, bool)> {
        (0..n)
            .map(|c| {
                let cell = &t.term().grid().screen()[Line(0)][Column(c)];
                (cell.c, cell.flags.contains(Flags::WIDE_CHAR))
            })
            .collect()
    }

    const CLUSTERS: &str = "🇮🇹 👍🏽 👨\u{200D}👩\u{200D}👧 ❤\u{FE0F}";

    #[test]
    fn clusters_with_mode_one_cell_each() {
        let mut t = Terminal::new(size(80, 24), 100);
        t.feed(format!("\x1b[?2027h{CLUSTERS}").as_bytes());
        let wide: Vec<char> = cells(&t, 11)
            .into_iter()
            .filter(|&(_, wide)| wide)
            .map(|(c, _)| c)
            .collect();
        assert_eq!(wide, ['🇮', '👍', '👨', '❤']);
        assert_eq!(t.term().grid().cursor.point.column, Column(11));
    }

    #[test]
    fn clusters_without_mode_unchanged() {
        let mut t = Terminal::new(size(80, 24), 100);
        t.feed(CLUSTERS.as_bytes());
        // Regional indicators and the family's members take cells of their own; zero-width
        // pieces attach to the cell before them: 2 + 1 + 4 + 1 + 6 + 1 + 1 columns.
        assert_eq!(t.term().grid().cursor.point.column, Column(16));
        assert_eq!(cells(&t, 2), [('🇮', false), ('🇹', false)]);
    }

    #[test]
    fn kitty_requests_ignored_when_setting_off() {
        let mut t = Terminal::new(size(80, 24), 100);
        assert!(!t.kitty_keyboard());
        t.feed(b"\x1b[>1u\x1b[=8;1u\x1b[?u");
        assert!(!t.mode().intersects(TermMode::KITTY_KEYBOARD_PROTOCOL));
        assert!(reply(&mut t).is_empty(), "no answer to the flags query");
        assert_eq!(esc(&t), b"\x1b");
    }

    #[test]
    fn kitty_push_query_set_pop_when_setting_on() {
        let mut t = Terminal::new(size(80, 24), 100);
        t.set_kitty_keyboard(true);
        t.feed(b"\x1b[>1u");
        assert!(t.mode().contains(TermMode::DISAMBIGUATE_ESC_CODES));
        assert_eq!(esc(&t), b"\x1b[27u");
        t.feed(b"\x1b[?u");
        assert_eq!(reply(&mut t), "\x1b[?1u");
        t.feed(b"\x1b[=10;2u");
        assert!(t.mode().contains(
            TermMode::DISAMBIGUATE_ESC_CODES
                | TermMode::REPORT_EVENT_TYPES
                | TermMode::REPORT_ALL_KEYS_AS_ESC
        ));
        t.feed(b"\x1b[<u");
        assert!(!t.mode().intersects(TermMode::KITTY_KEYBOARD_PROTOCOL));
        assert_eq!(esc(&t), b"\x1b");
    }

    #[test]
    fn turning_kitty_off_drops_active_flags() {
        let mut t = Terminal::new(size(80, 24), 100);
        t.feed(b"\x1b]0;keep\x07");
        t.take_events();
        t.feed(b"\x1b]0;pending\x07");
        t.set_kitty_keyboard(true);
        assert_eq!(
            t.take_events(),
            vec![TermEvent::Title("pending".into())],
            "a title set before the toggle is kept, the re-announcement dropped"
        );
        t.feed(b"\x1b[>31u");
        assert!(t.mode().contains(TermMode::KITTY_KEYBOARD_PROTOCOL));
        t.set_kitty_keyboard(false);
        assert!(!t.mode().intersects(TermMode::KITTY_KEYBOARD_PROTOCOL));
        assert!(t.take_events().is_empty(), "no spurious title event");
        assert_eq!(esc(&t), b"\x1b");
    }

    /// Depth of the flag stack, measured by popping: pops `n` and reports whether any
    /// flags remain. Leaves the stack shorter.
    fn flags_after_popping(t: &mut Terminal, n: u16) -> bool {
        t.feed(format!("\x1b[<{n}u").as_bytes());
        t.mode().intersects(TermMode::KITTY_KEYBOARD_PROTOCOL)
    }

    fn pushes(n: usize) -> Vec<u8> {
        b"\x1b[>1u".repeat(n)
    }

    /// Regression test for the feathercritty fix (its README, change 1): a full
    /// flag stack evicts its oldest entry instead of panicking.
    #[test]
    fn flag_stack_floods_do_not_crash_and_stay_capped() {
        let mut t = Terminal::new(size(80, 24), 100);
        t.set_kitty_keyboard(true);
        let flood = pushes(5000);
        t.feed(&flood);
        assert!(t.mode().contains(TermMode::DISAMBIGUATE_ESC_CODES));
        // Capped at 4096 entries: popping 4095 leaves one, the next pop empties it.
        assert!(flags_after_popping(&mut t, 4095));
        assert!(!flags_after_popping(&mut t, 1));
        // One byte at a time.
        for chunk in flood.chunks(1) {
            t.feed(chunk);
        }
        assert!(flags_after_popping(&mut t, 4095));
        assert!(!flags_after_popping(&mut t, 1));
    }

    #[test]
    fn screen_swaps_cannot_grow_the_flag_stack_past_its_cap() {
        let mut t = Terminal::new(size(80, 24), 100);
        t.set_kitty_keyboard(true);
        // `CSI ? 1049 ; 0 ; 0 ... h` with 31 extra parameters still swaps screens.
        let extra = ";0".repeat(31);
        let enter = format!("\x1b[?1049{extra}h");
        let leave = format!("\x1b[?1049{extra}l");
        let burst = pushes(128);
        for _ in 0..40 {
            t.feed(&burst);
            t.feed(enter.as_bytes());
            t.feed(b"\x1b[<128u");
            t.feed(leave.as_bytes());
        }
        assert!(!t.mode().contains(TermMode::ALT_SCREEN));
        // Main screen holds 40 * 128 = 5120 pushes, capped at 4096.
        assert!(flags_after_popping(&mut t, 4095));
        assert!(!flags_after_popping(&mut t, 1));
    }

    /// A sixel on screen, then the setting switched off: the placement is gone at once and its
    /// image released exactly once; switching it back on does not bring it back.
    #[test]
    fn switch_off_clears_images() {
        let mut t = Terminal::new(size(80, 24), 100);
        // Cells of 8x16 px: the picture below covers one cell.
        t.set_cell_pixel_size(8, 16);
        assert!(t.images_enabled(), "on by default");
        // A red 4x12 px sixel: four painted columns, then a second band of four.
        t.feed(b"\x1bPq#0;2;100;0;0#0~~~~-~~~~\x1b\\");
        let keys: Vec<ImageKey> = t
            .take_events()
            .iter()
            .filter_map(|e| match e {
                TermEvent::ImageReceived { key, data } => {
                    match data {
                        ImageData::Rgba { width, height, .. } => {
                            assert_eq!((*width, *height), (4, 12))
                        }
                        ImageData::Encoded { .. } => panic!("a sixel's pixels arrive decoded"),
                    }
                    Some(*key)
                }
                _ => None,
            })
            .collect();
        assert_eq!(keys.len(), 1, "one picture arrives");
        let key = keys[0];
        assert_eq!(
            t.term().visible_placements().len(),
            1,
            "and it is on screen"
        );
        t.set_images(false, false);
        assert!(!t.images_enabled());
        assert!(!t.scrollback_images());
        assert!(
            t.term().visible_placements().is_empty(),
            "switching off clears what is on screen"
        );
        assert_eq!(
            t.take_events(),
            vec![TermEvent::ImageReleased(key)],
            "the picture is released exactly once"
        );
        t.set_images(true, false);
        assert!(
            t.term().visible_placements().is_empty(),
            "switching back on does not resurrect it"
        );
        assert!(t.take_events().is_empty());
    }

    #[test]
    fn title_and_bell_events() {
        let mut t = Terminal::new(size(80, 24), 100);
        t.feed(b"\x1b]0;hello\x07\x07");
        let ev = t.take_events();
        assert!(ev.contains(&TermEvent::Title("hello".into())));
        assert!(ev.contains(&TermEvent::Bell));
    }
}
