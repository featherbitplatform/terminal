//! The app-wide store of decoded images: caps, eviction, resample cache and the worker thread.
//!
//! One [`ImageStore`] serves every terminal in the process ([`store`]); tests build their own with
//! small caps through [`ImageStore::with_caps`]. Images are keyed by an *owner id* per
//! [`Terminal`] (fbterm-term cannot know the app's pane ids, and a reconnect feeds the same
//! terminal through a new session), and each owner registers [`OwnerHooks`] so finished work can
//! reach its session: the worker reports decode results to the terminal (which answers kitty
//! clients) and then calls the owner's `deliver` hook, which takes those events out of the terminal
//! and routes them in the order it finds them; `wake` tells the pane to redraw.
//!
//! One thread, `fbterm-image-worker`, does all decoding and resampling — never on the UI thread,
//! never while the store lock is held, and it sleeps on a condition variable when idle (it never
//! ticks). Work arrives through a queue like [`crate::history_worker`].
//!
//! Caps: decoded bytes per owner ([`PER_OWNER_CAP`](crate::images::PER_OWNER_CAP)) and app-wide
//! ([`TOTAL_CAP`](crate::images::TOTAL_CAP)); resampled copies count too. A submit that pushes a
//! cap over is *reported* by [`ImageStore::over_cap`], not silently fixed: the session first asks
//! its terminal to turn its oldest scrollback placements into placeholders
//! (`Terminal::evict_oldest_scrollback_image`), releases what that frees, and only then does
//! [`ImageStore::enforce_caps`] drop buffers directly — least recently shown first, in this order:
//! this owner's resampled copies, its whole images, other owners' resamples, their whole images.
//! The store never locks a terminal while holding its own mutex; taking the store lock under a
//! terminal lock is allowed.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use std::time::{Duration, Instant};

use feathercritty::event::{ImageData, ImageDecodeError, ImageKey};
use feathercritty::graphics::kitty::Rect;
use iced::advanced::image::Handle;
use parking_lot::{Condvar, Mutex};

#[cfg(test)]
use crate::TermEvent;
use crate::Terminal;
use crate::images::decode::{self, DecodeError, DecodeHook, Rgba8Image};
use crate::images::resample::{is_integer_multiple, resample};
use crate::images::{PER_OWNER_CAP, TOTAL_CAP};

/// What a session registered for one terminal, so finished image work reaches it.
///
/// `term` is weak on purpose: the store must not keep a closed pane's terminal alive, and nothing
/// a hook captures may hold a strong terminal reference either (the hooks live in the store).
/// Registering again with the same owner id replaces the hooks — that is how a reconnect retargets
/// decode results to its new session while the images survive.
pub struct OwnerHooks {
    pub term: Weak<Mutex<Terminal>>,
    /// Called (on the worker thread, with no store lock and no terminal lock held) once a finished
    /// decode has queued its answer in the terminal's event queue. It takes nothing: whoever routes
    /// terminal events empties that queue itself, under its own lock, so an answer joins the replies
    /// produced by output instead of being carried past them (see [`ImageStore::deliver`]).
    pub deliver: Arc<dyn Fn() + Send + Sync>,
    /// Called when new pixels became available, so the pane redraws (it is `mark_dirty`).
    pub wake: Arc<dyn Fn() + Send + Sync>,
}

/// The three hook fields of [`OwnerHooks`] cloned out together, ready to run with no store lock
/// held (what [`ImageStore::deliver`] hands an answer to).
type DeliveryHooks = (
    Weak<Mutex<Terminal>>,
    Arc<dyn Fn() + Send + Sync>,
    Arc<dyn Fn() + Send + Sync>,
);

/// A resampled copy of an image at one placement size, ready to draw.
///
/// Its pixels live in an iced image [`Handle`] built once on the worker when the resample is
/// installed (see [`ImageStore::finish_resample`]): iced uploads a handle to the GPU the first
/// time it is drawn and reuses the texture for every later frame that draws the same handle, so
/// one resample means one upload however often the pane redraws. The drawer clones this handle per
/// frame; when a copy is dropped or simply stops being drawn, iced frees the texture itself.
#[derive(Debug)]
pub struct ResampledImage {
    pub width: u32,
    pub height: u32,
    /// The source image's own pixel size, so a drawer can map a placement's `src` rectangle onto
    /// this copy (see [`crate::widget::images::crop_for_src`]).
    pub source: (u32, u32),
    /// The copy's pixels, straight (non-premultiplied) RGBA8, as an iced handle.
    pub handle: Handle,
}

impl ResampledImage {
    fn bytes(&self) -> usize {
        // The same number the pixel buffer used to report: one RGBA8 frame of `width * height`.
        (self.width as usize)
            .saturating_mul(self.height as usize)
            .saturating_mul(4)
    }

    /// Whether this copy IS the picture's own pixels rather than a frame the worker filtered: an
    /// ask at exactly the source size aliases the source buffer (see [`Job::Resample`]), so it
    /// accounts nothing and evicting it would free nothing.
    fn shares_source(&self) -> bool {
        self.width == self.source.0 && self.height == self.source.1
    }

    /// Bytes this copy adds to the caps: a full frame, except an alias of an accounted source,
    /// which adds none (finding P4). Every accounting site uses this, never [`Self::bytes`].
    fn charge(&self) -> usize {
        if self.shares_source() {
            0
        } else {
            self.bytes()
        }
    }

    /// The copy's pixels, read back out of the handle (tests: what a golden buffer must match).
    #[cfg(test)]
    pub(crate) fn pixels(&self) -> Vec<u8> {
        match &self.handle {
            Handle::Rgba { pixels, .. } => pixels.to_vec(),
            other => panic!("a resample holds RGBA pixels, got {other:?}"),
        }
    }
}

/// What [`ImageStore::get`] found for one placement request.
#[derive(Debug)]
pub enum Lookup {
    /// A resample at exactly the asked size is ready to draw.
    Ready(Arc<ResampledImage>),
    /// The image is known but its decode or a requested resample is still running: draw nothing
    /// and redraw when [`OwnerHooks::wake`] fires.
    Pending,
    /// The decoded buffer was dropped by the caps (or the size cannot be made beside it), so the
    /// placement draws its placeholder. `width` and `height` are the source image's pixel size:
    /// what the `[image W×H]` text says (spec 2.6).
    Evicted { width: u32, height: u32 },
    /// Never seen, or released: nothing to draw and no reason to wait.
    Unknown,
}

/// Why a resampled copy was refused: the two situations need different fates.
///
/// [`Refusal::Geometry`] is permanent: this size provably cannot coexist with its own source
/// under the tightest cap, so nothing that frees other memory can change it: it stays refused
/// until the source is replaced or released. [`Refusal::Pressure`] is not: a fresh install
/// displaced this copy only to make room (see [`Reason::Install`]), so the refusal is cleared as
/// soon as memory frees: the next release or dropped owner that actually removed something
/// removes the slot, and the drawer's next ask for that size queues a resample as for any
/// uncached one. The refusal exists either way to stop two copies that cannot coexist evicting
/// each other every frame (the loop the round-3 review calls N4): a size refused under pressure
/// stays refused until the next release, so the two copies race once more and settle again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Refusal {
    Geometry,
    Pressure,
}

/// One cached resample of one image: queued, ready with its last-used stamp, or refused.
enum Slot {
    Queued,
    Ready(u64, Arc<ResampledImage>),
    /// This size has no copy: either it cannot fit beside its own source under the caps at all
    /// ([`Refusal::Geometry`], see [`ImageStore::finish_resample`]), or another image's
    /// install-time enforcement evicted it to make room, its whole image still being held (see
    /// [`Reason::Install`]). Draws answer [`Lookup::Evicted`] for a refused size either way
    /// (spec 6.3's placeholder rule). The two kinds differ only in what ends the refusal: a
    /// pressure refusal is removed by a release or a dropped owner that actually removed
    /// something (`ImageStore::release` and [`ImageStore::drop_owner`]), after which the size is
    /// asked afresh on the next draw; a geometry refusal stays until the source changes. Copies
    /// dropped by ordinary submit- or decode-time enforcement stay re-makeable (plain LRU): they
    /// never become `Refused`.
    Refused(Refusal),
}

/// The state of one image key.
enum State {
    /// A container payload is queued or decoding; its size counts against caps until it resolves.
    Pending(usize),
    Ready(Arc<Rgba8Image>),
    /// The buffer was dropped by the caps; the key stays known until its release, so `get` can
    /// answer [`Lookup::Evicted`] instead of pretending the image never existed.
    Evicted,
}

/// The resample cache key: a copy's pixel size and whether it was asked to fill its box rather than
/// letterbox into it (see [`ImageStore::get`]'s `stretch`). A stretched and a kept-aspect ask of one
/// span are different pictures, so they never share a slot.
type ResampleKey = (u32, u32, bool);

fn resample_key(target: (u32, u32), stretch: bool) -> ResampleKey {
    (target.0, target.1, stretch)
}

struct Entry {
    state: State,
    /// Last time this whole image was drawn or kept alive by a draw (LRU).
    shown: u64,
    /// The source image's pixel size, known from the payload before any decoding (feathercritty
    /// reports the header's dimensions with every `Encoded` transmission): what an evicted drawing
    /// names in its `[image W×H]` placeholder.
    source: (u32, u32),
    resamples: HashMap<ResampleKey, Slot>,
    resample_bytes: usize,
    /// Exact identity of this entry: jobs carry the serial of the submit they came from, and an
    /// answer is only delivered for the entry it belongs to (see [`Job::Refuse`]'s rule).
    serial: u64,
    /// An `Encoded` submit owes the emulator exactly one `image_decoded` call. feathercritty holds
    /// the client's reply and one of its 64 pending-decode slots until it arrives, so every key
    /// must be answered exactly once even when the caps get in the way: the decode result, or a
    /// refusal queued in its place (see [`Job::Refuse`]). An answer is only owed while it has not
    /// actually been delivered — with no hooks registered it waits in `pending_answer` instead of
    /// being dropped (see [`Job::Answer`]). `release` and `drop_owner` clear it silently because
    /// nothing waits anymore — the terminal is gone or has forgotten the image.
    owes_reply: bool,
    /// A decode result or refusal that was ready to deliver while no hooks were registered; a
    /// later [`ImageStore::register_owner`] queues a [`Job::Answer`] to hand it over.
    pending_answer: Option<Result<(), ImageDecodeError>>,
}

impl Entry {
    fn accounted(&self) -> usize {
        let base = match &self.state {
            State::Pending(payload) => *payload,
            State::Ready(img) => img.bytes(),
            State::Evicted => 0,
        };
        base + self.resample_bytes
    }
}

/// What a store operation took out while its mutex was held: buffers, jobs, and the owners whose
/// visible state changed.
///
/// A few hundred MiB of pixel buffers and up to 16 MiB of queued payloads are freed *after* the
/// lock is released (see [`ImageStore::finish_resample`]), so dropping them cannot stall a draw-path
/// `get`. `wake` names owners whose state changed in a way their pane must see — a size refused, a
/// copy or picture they were shown evicted, a size refused under pressure that a release just
/// brought back: the cached draw list holds a handle for pixels that are gone now, and asking
/// again answers the placeholder spec 6.3 asks for. Those hooks run with no lock held (see
/// [`ImageStore::wake_owners`]).
#[derive(Default)]
struct Freed {
    jobs: Vec<Job>,
    entries: Vec<Entry>,
    images: Vec<Arc<Rgba8Image>>,
    copies: Vec<Arc<ResampledImage>>,
    wake: Vec<u64>,
}

impl Freed {
    fn take_jobs(inner: &mut Inner, keep: impl Fn(&Job) -> bool, freed: &mut Freed) {
        let mut kept = VecDeque::with_capacity(inner.jobs.len());
        while let Some(job) = inner.jobs.pop_front() {
            if keep(&job) {
                kept.push_back(job);
            } else {
                freed.jobs.push(job);
            }
        }
        inner.jobs = kept;
    }

    /// Everything an entry held, ready to drop outside the lock.
    fn drain_entry(&mut self, mut entry: Entry) {
        self.drain_resamples(std::mem::take(&mut entry.resamples));
        self.entries.push(entry);
    }

    /// The pixel buffers of one evicted image, leaving its (now `Evicted`) entry in place.
    fn drain_resamples(&mut self, resamples: HashMap<ResampleKey, Slot>) {
        for (_, slot) in resamples {
            if let Slot::Ready(_, out) = slot {
                self.copies.push(out);
            }
        }
    }

    /// Notes that `owner`'s visible state changed, so its pane has to be asked to redraw. Duplicates
    /// are harmless: [`ImageStore::wake_owners`] wakes each owner once.
    fn woke(&mut self, owner: u64) {
        self.wake.push(owner);
    }

    /// Records that `owner`'s frame is now stale AND bumps its generation — the pair every caller
    /// wants (see [`Freed::woke`] and [`ImageStore::generation`]). Call under the store lock; the
    /// bump landing before the wake goes out is what guarantees a rebuild woken by it sees the new
    /// number. Use bare `woke` where nothing was taken from the pane (a satisfied install: its
    /// asking frame parked on `pending` and rebuilds anyway).
    fn changed(&mut self, inner: &mut Inner, owner: u64) {
        if let Some(entry) = inner.owners.get_mut(&owner) {
            entry.generation += 1;
        } // gone with its entry (terminal dropped or released): nothing to invalidate, and
        // `wake_owners` finds no hook for it either.
        self.wake.push(owner);
    }

    /// Bring back every size a release just made room for: remove each
    /// [`Slot::Refused(Refusal::Pressure)`] across ALL owners still in the store (refusals cross
    /// panes) so the drawer's next ask for that size finds no slot and queues a resample as for
    /// any uncached size, and record each owner that lost one through [`Self::changed`] so its
    /// pane redraws. Call under the store lock, in the same section as the removal, and only when
    /// that removal actually took something: a second release of the same key, or a release of an
    /// unknown key, must clear nothing and wake nothing. A geometry refusal is untouched: it
    /// provably cannot fit beside its own source, and freeing other memory cannot change that.
    ///
    /// The slots are REMOVED, not queued: nothing here may touch the worker's queue, because the
    /// caller may not be in a position to wake the worker, and a `Queued` slot whose job never
    /// runs is a `Pending` the drawer waits on forever. The cleared owner is woken (after the
    /// lock), its generation moves, and its redraw's `get` takes the plain uncached path: it
    /// pushes the `Job::Resample` and notifies the worker there. Refusal slots account no bytes,
    /// so no ledger moves.
    fn clear_pressure_refusals(&mut self, inner: &mut Inner) {
        // Collect the owners first: bumping a generation re-borrows `inner.owners`, so the
        // mutations must be done before the `changed` calls.
        let mut cleared = Vec::new();
        for (owner, entry) in &mut inner.owners {
            for image in entry.images.values_mut() {
                image.resamples.retain(|_, slot| {
                    let pressure = matches!(slot, Slot::Refused(Refusal::Pressure));
                    if pressure {
                        cleared.push(*owner);
                    }
                    !pressure
                });
            }
        }
        for owner in cleared {
            self.changed(inner, owner);
        }
    }
}

fn new_owner() -> OwnerEntry {
    OwnerEntry {
        hooks: None,
        images: HashMap::new(),
        bytes: 0,
        generation: 0,
    }
}

struct OwnerEntry {
    hooks: Option<OwnerHooks>,
    images: HashMap<ImageKey, Entry>,
    /// Payloads waiting to decode, decoded buffers and resampled copies of this owner.
    bytes: usize,
    /// Bumped whenever anything this owner was shown goes (an eviction) or a size it could ask for
    /// becomes `Refused` — exactly where [`Freed::wake`] records it. A wake says "redraw"; this
    /// counter says why: the pane's cached frame holds handles for pixels that are gone, and it is
    /// part of the widget's frame-cache key so that redraw rebuilds the frame instead of painting
    /// the stale one (which would keep those very bytes alive outside the store's books). Reads are
    /// cheap and lock-free-ish for the drawer ([`ImageStore::generation`]); a never-seen owner reads
    /// 0, and un-registering an owner drops its counter with its entry.
    generation: u64,
}

/// One unit of worker-thread work. Queued jobs keep the payload or source they need, so running
/// them touches no lock until the result comes back.
///
/// **Exactly-once rule for an `Encoded` key's answer** (feathercritty waits for one
/// `image_decoded` per encoded transmission): every entry that owes one carries `owes_reply`, and
/// whichever job claims it first delivers it — a claim is only honored when the job's `serial`
/// still matches the entry's, so an answer from a replaced entry never lands. When the caps evict
/// a key whose decode has not run yet, `evict_image` replaces its queued `Decode` with a `Refuse`;
/// when the decode is already running it queues a `Refuse` as well, and then exactly one of the
/// two claims: usually the running decode does (it finds the entry `Evicted` and answers
/// `TooLarge`) and the `Refuse` sees the obligation gone — but if the worker happens to run the
/// `Refuse` first, that one answers and the decode result is dropped. When no hooks are registered
/// at claim time the answer is not lost either: it waits in the entry (`Entry::pending_answer`),
/// the obligation stays open, and `register_owner` queues a `Job::Answer` that claims and delivers
/// it — still exactly once, because delivering clears `owes_reply` under the same serial check. A
/// key is never answered twice, and never left unanswered while its entry lives.
enum Job {
    Decode {
        owner: u64,
        key: ImageKey,
        serial: u64,
        data: ImageData,
    },
    Resample {
        owner: u64,
        key: ImageKey,
        target: (u32, u32),
        /// Whether this copy fills its box instead of being fitted into it (see [`ImageStore::get`]):
        /// part of the cache key, so a fill and a fit of one span are two slots.
        stretch: bool,
        integer: bool,
        src: Arc<Rgba8Image>,
    },
    /// Answer an evicted key `Err(TooLarge)` on the worker, through the same path a decode result
    /// takes. Queued by `evict_image` in place of (or besides) the decode it removed; see the
    /// exactly-once rule above.
    Refuse {
        owner: u64,
        key: ImageKey,
        serial: u64,
    },
    /// Deliver an answer that was ready while its owner had no hooks (see the exactly-once rule
    /// above). Queued by `register_owner` for every owed key of that owner with an answer waiting.
    Answer {
        owner: u64,
        key: ImageKey,
        serial: u64,
    },
}

/// What a [`Job::Resample`] brings back to install: the frame the worker filtered, or — when the
/// size asked for is the picture's own — an iced handle over the source buffer itself. Sharing it
/// costs one refcount on bytes that are already accounted, and iced uploads the one texture those
/// pixels have (finding P4); a picture drawn both 1:1 and scaled pays for a single frame.
enum Copy {
    Made(Rgba8Image),
    Shares(Handle),
}

#[derive(Default)]
struct Inner {
    owners: HashMap<u64, OwnerEntry>,
    /// Sum of every owner's accounted bytes.
    total: usize,
    jobs: VecDeque<Job>,
    /// A job is popped and running on the worker thread.
    busy: bool,
    per_owner_cap: usize,
    total_cap: usize,
}

/// Decoded images for every terminal in the app (see the module docs).
pub struct ImageStore {
    inner: Mutex<Inner>,
    /// Signals the worker that a job arrived.
    wake: Condvar,
    /// Signalled (with the lock held) when nothing is queued and no job runs.
    idle: Condvar,
    /// Whether the worker thread runs (see [`ImageStore::start`]).
    running: AtomicBool,
    /// Monotonic LRU stamps, kept out of `Inner` so short borrows never fight over it.
    stamper: AtomicU64,
    /// A test's decoder override (see [`decode::decode_with`]); production leaves it `None`.
    decode_hook: Mutex<Option<DecodeHook>>,
}

impl ImageStore {
    /// A store with explicit caps; the process-wide one uses 256 MiB per owner and 512 MiB total.
    pub fn with_caps(per_owner: usize, total: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                owners: HashMap::new(),
                total: 0,
                jobs: VecDeque::new(),
                busy: false,
                per_owner_cap: per_owner,
                total_cap: total,
            }),
            wake: Condvar::new(),
            idle: Condvar::new(),
            running: AtomicBool::new(false),
            stamper: AtomicU64::new(0),
            decode_hook: Mutex::new(None),
        }
    }

    #[cfg(test)]
    pub(crate) fn set_decode_hook(&self, hook: Option<DecodeHook>) {
        *self.decode_hook.lock() = hook;
    }

    /// Attach (or re-attach, after a reconnect) a terminal's session hooks, and hand over any
    /// answers that finished while this owner had none (queued as [`Job::Answer`] for the worker).
    pub fn register_owner(&self, owner: u64, hooks: OwnerHooks) {
        let mut inner = self.inner.lock();
        let waiting: Vec<(ImageKey, u64)> = {
            let entry = inner.owners.entry(owner).or_insert_with(new_owner);
            entry.hooks = Some(hooks);
            entry
                .images
                .iter()
                .filter(|(_, image)| image.owes_reply && image.pending_answer.is_some())
                .map(|(key, image)| (*key, image.serial))
                .collect()
        };
        let queued = !waiting.is_empty();
        for (key, serial) in waiting {
            inner.jobs.push_back(Job::Answer { owner, key, serial });
        }
        drop(inner);
        if queued {
            self.wake.notify_one();
            self.start_global_worker();
        }
    }

    /// Forget everything one terminal had, including queued work. Called from `Terminal::drop`,
    /// so nothing is answered: the terminal — and every client waiting on it — is gone.
    ///
    /// Removing an owner that existed frees its bytes, so the sizes other panes had to refuse
    /// under that pressure come back with it: every `Refused(Pressure)` slot is cleared in the
    /// same lock section as the removal, and the owners that lost one are woken after the lock
    /// goes, so their next draw asks the size afresh (see
    /// [`Freed::clear_pressure_refusals`]). A second drop of the same owner removes nothing and
    /// clears nothing. The owner being dropped is never woken: its hooks are gone.
    pub fn drop_owner(&self, owner: u64) {
        let mut freed = Freed::default();
        // The session hooks die with the owner; they are dropped after the lock too (they hold the
        // session's senders, never the terminal, but nothing belongs inside a short section).
        let mut hooks = Vec::new();
        {
            let mut inner = self.inner.lock();
            let gone = match inner.owners.remove(&owner) {
                Some(entry) => {
                    inner.total -= entry.bytes;
                    hooks.extend(entry.hooks);
                    for (_, image) in entry.images {
                        freed.drain_entry(image);
                    }
                    true
                }
                None => false,
            };
            if gone {
                freed.clear_pressure_refusals(&mut inner);
            }
            Freed::take_jobs(&mut inner, |job| job_owner(job) != owner, &mut freed);
            Self::check_idle(&inner, &self.idle);
        }
        if !freed.wake.is_empty() {
            self.wake_owners(&freed.wake);
        }
    }

    /// Take ownership of one image payload the terminal handed over.
    ///
    /// `Rgba` pixels (sixel output, raw kitty RGB/RGBA) are stored directly: the terminal decoded
    /// them and already answered the client, so no decode job and no callback follow. A payload
    /// whose byte count does not match its declared dimensions is dropped — a 0-wide or truncated
    /// frame has nothing to draw and would only poison the resample path. `Encoded` payloads queue
    /// a decode on the worker; its result goes back to the terminal through the owner's hooks,
    /// which is what answers a kitty client waiting for `OK` or an error.
    ///
    /// A submit may push the caps over on purpose: it does not evict anything itself (the session
    /// first gets its chance to reclaim scrollback properly — see [`Self::over_cap`] and
    /// [`crate::images::reclaim`]). Decode completions run the same reclaim themselves.
    pub fn submit(&self, owner: u64, key: ImageKey, data: ImageData) {
        let stamp = self.next_stamp();
        let serial = self.next_stamp();
        let mut inner = self.inner.lock();
        // Each branch mutates the owner entry first and only then touches `inner` again: the
        // entry's borrow ends with its last use, so the totals can be adjusted afterwards.
        if let ImageData::Rgba {
            width,
            height,
            pixels,
        } = data
        {
            if width == 0 || height == 0 || pixels.len() != (width as usize) * (height as usize) * 4
            {
                return; // malformed for its declared size: store nothing, answer nothing
            }
            let img = Arc::new(Rgba8Image {
                width,
                height,
                pixels: pixels.into(),
            });
            let bytes = img.bytes();
            let entry = inner.owners.entry(owner).or_insert_with(new_owner);
            // A key replaced (the emulator never reuses one; defensive anyway): the old entry's
            // buffers go after the lock, and its owed reply — if any — goes with it.
            let gone = entry
                .images
                .insert(
                    key,
                    Entry {
                        state: State::Ready(img),
                        shown: stamp,
                        source: (width, height),
                        resamples: HashMap::new(),
                        resample_bytes: 0,
                        serial,
                        owes_reply: false,
                        pending_answer: None,
                    },
                )
                .map(|old| {
                    let accounted = old.accounted();
                    drop(old);
                    accounted
                })
                .unwrap_or(0);
            entry.bytes = entry.bytes - gone + bytes;
            inner.total = inner.total - gone + bytes;
            return;
        }
        let (payload, source) = match &data {
            ImageData::Encoded {
                bytes,
                width,
                height,
                ..
            } => (bytes.len(), (*width, *height)),
            ImageData::Rgba { .. } => unreachable!(),
        };
        let entry = inner.owners.entry(owner).or_insert_with(new_owner);
        let gone = entry
            .images
            .insert(
                key,
                Entry {
                    state: State::Pending(payload),
                    shown: stamp,
                    source,
                    resamples: HashMap::new(),
                    resample_bytes: 0,
                    serial,
                    owes_reply: true,
                    pending_answer: None,
                },
            )
            .map(|old| {
                let accounted = old.accounted();
                drop(old);
                accounted
            })
            .unwrap_or(0);
        entry.bytes = entry.bytes - gone + payload;
        inner.total = inner.total - gone + payload;
        inner.jobs.push_back(Job::Decode {
            owner,
            key,
            serial,
            data,
        });
        drop(inner);
        self.wake.notify_one();
        self.start_global_worker();
    }

    /// Forget one image entirely (its [`crate::TermEvent::ImageReleased`]). Idempotent: the terminal
    /// also releases keys that scrollback eviction or a failed decode already dropped from the store,
    /// and a second release of the same key must stay harmless. Nothing is answered for a released
    /// key: the terminal has forgotten it, client included.
    ///
    /// A release that actually removed an image frees memory, so sizes other panes had to refuse
    /// under that pressure come back with it: every `Refused(Pressure)` slot across the store is
    /// cleared in the same lock section as the removal, and each owner that lost one is woken
    /// after the lock goes, so its next draw asks the size afresh (see
    /// [`Freed::clear_pressure_refusals`]). A second release of the same key, or a release of an
    /// unknown key, removes nothing, clears nothing and wakes nothing.
    pub fn release(&self, owner: u64, key: ImageKey) {
        let mut freed = Freed::default();
        {
            let mut inner = self.inner.lock();
            let mut removed = false;
            if let Some(entry) = inner.owners.get_mut(&owner)
                && let Some(image) = entry.images.remove(&key)
            {
                let gone = image.accounted();
                entry.bytes -= gone;
                inner.total -= gone;
                freed.drain_entry(image);
                removed = true;
            }
            if removed {
                freed.clear_pressure_refusals(&mut inner);
            }
            Freed::take_jobs(&mut inner, |job| !job_is(job, owner, key), &mut freed);
            Self::check_idle(&inner, &self.idle);
        }
        if !freed.wake.is_empty() {
            self.wake_owners(&freed.wake);
        }
    }

    /// Ask for the resample that draws in a placement covering `box_px` PHYSICAL pixels — the
    /// placement's cell span at the cell size programs are told (see
    /// [`crate::widget::images::placement_box_px`]), so what is drawn fills exactly the cells the
    /// program asked for at any font size or display scale.
    ///
    /// `src` is the placement's source rectangle (`None`: the whole image). The fit factor comes
    /// from that rectangle, so a cropped placement fills its box as an uncropped one would; the
    /// copy made is always of the WHOLE image at that factor and is keyed by its size alone, so
    /// placements cropping the same shape of one image share it, and the drawer maps `src` onto it
    /// with [`ResampledImage::source`] (see [`crate::widget::images::crop_for_src`]).
    ///
    /// `stretch` is the protocol's fill request: an OSC 1337 picture sent with
    /// `preserveAspectRatio=0` and both axes sized asks to fill its span, so the copy is made at
    /// exactly `box_px` and the picture's own ratio is ignored. Everything else (kitty `c` with `r`,
    /// kitty placeholders, sixel, and OSC 1337 with the default `preserveAspectRatio=1`) passes
    /// false and is fitted into the box as before. A stretched ask never shares a slot with a
    /// kept-aspect ask of the same span (see [`ResampleKey`]).
    ///
    /// A size not cached yet is queued on the worker and answers [`Lookup::Pending`]: nothing is
    /// ever resampled synchronously here, because `get` runs on the draw path. When the resample
    /// finishes, the owner's `wake` fires and a later `get` returns it.
    pub fn get(
        &self,
        owner: u64,
        key: ImageKey,
        box_px: (u32, u32),
        src: Option<Rect>,
        stretch: bool,
    ) -> Lookup {
        let mut inner = self.inner.lock();
        let Some(owner_entry) = inner.owners.get_mut(&owner) else {
            return Lookup::Unknown;
        };
        let Some(image) = owner_entry.images.get_mut(&key) else {
            return Lookup::Unknown;
        };
        let source_size = image.source;
        let whole = match &image.state {
            State::Ready(img) => img.clone(),
            State::Pending(_) => return Lookup::Pending,
            State::Evicted => {
                return Lookup::Evicted {
                    width: source_size.0,
                    height: source_size.1,
                };
            }
        };
        // Fitted into the box keeping the ratio, or the box itself when the protocol asked for a fill.
        let target = if stretch {
            fill_target(box_px)
        } else {
            fit_target(&whole, src, box_px)
        };
        let slot_key = resample_key(target, stretch);
        // The filter rule is asked about the pair the resample actually scales: `whole` to `target`
        // (the crop's own area would call an exact 2x zoom of a sheet with a different-aspect tile
        // non-integer, and pixel art would go through Lanczos for no reason).
        image.shown = self.next_stamp();
        match image.resamples.get_mut(&slot_key) {
            Some(Slot::Ready(stamp, out)) => {
                *stamp = self.next_stamp();
                return Lookup::Ready(out.clone());
            }
            // already queued with this draw or the last one
            Some(Slot::Queued) => return Lookup::Pending,
            // either kind of refusal answers the same way for both: the placeholder (see
            // `finish_resample` and [`Reason::Install`]); a pressure refusal comes back when a
            // release frees memory, a geometry refusal does not
            Some(Slot::Refused(_)) => {
                return Lookup::Evicted {
                    width: source_size.0,
                    height: source_size.1,
                };
            }
            None => {}
        }
        let integer = is_integer_multiple(whole.width, whole.height, target.0, target.1);
        image.resamples.insert(slot_key, Slot::Queued);
        // This picture's other cached sizes stay: a drawer that later comes back to one of them
        // should not have to re-make it, and memory is bounded by the LRU plus cap enforcement at
        // install time (see `finish_resample`), not by dropping siblings here.
        inner.jobs.push_back(Job::Resample {
            owner,
            key,
            target,
            stretch,
            integer,
            // The WHOLE decoded image: the crop is the drawer's job (see [`fit_target`]), so every
            // placement that fits at this factor shares this one copy and one GPU upload.
            src: whole,
        });
        drop(inner);
        self.wake.notify_one();
        self.start_global_worker();
        Lookup::Pending
    }

    /// Note that this image was drawn this frame, for LRU eviction (every draw calls it).
    pub fn touch(&self, owner: u64, key: ImageKey) {
        let mut inner = self.inner.lock();
        if let Some(image) = inner
            .owners
            .get_mut(&owner)
            .and_then(|o| o.images.get_mut(&key))
        {
            image.shown = self.next_stamp();
        }
    }

    /// Whether this owner's last submit pushed a cap over, so the session should reclaim
    /// (scrollback first) before drawing. True also when the app-wide total is over: whichever
    /// way it was crossed, [`Self::enforce_caps`] brings both back under.
    pub fn over_cap(&self, owner: u64) -> bool {
        let inner = self.inner.lock();
        Self::over(&inner, owner)
    }

    /// The picture's own pixel size as this store holds it (`None` when the owner or key is gone).
    ///
    /// For a payload still waiting to decode this is the size its header declared — the same
    /// numbers an evicted placement names in its `[image W×H]` placeholder; after a decode it is
    /// what that decode produced. A drawer asks for exactly this box to be drawn at 1:1 instead of
    /// filtered (finding I1), and such an ask is answered with the source's own bytes (finding P4).
    pub fn source_size(&self, owner: u64, key: ImageKey) -> Option<(u32, u32)> {
        let inner = self.inner.lock();
        Some(inner.owners.get(&owner)?.images.get(&key)?.source)
    }

    /// Drop buffers until the caps hold again (the step after scrollback eviction ran dry or was
    /// impossible): least recently shown first — this owner's resampled copies, then its whole
    /// images, then other owners' resamples, then their whole images. Whole images become
    /// [`Lookup::Evicted`] for later draws; released keys stay gone entirely. Any refusal this
    /// queues for an evicted owed key is followed by a worker wake (see [`Job::Refuse`]).
    ///
    /// Affected owners' `wake` hooks run here, with the store mutex already released, which is what
    /// a caller holding no other lock wants. A caller that holds a lock of its own — a session router
    /// holding its routing table — wants [`Self::collect_caps_reclaim`] instead: nothing may wake a
    /// pane while a lock it could be waiting on is held (finding M3).
    pub fn enforce_caps(&self, owner: u64) {
        let wake = self.collect_caps_reclaim(owner);
        self.wake_owners(&wake);
    }

    /// [`Self::enforce_caps`] with the redraw notices left to the caller: returns the owners whose
    /// visible state changed instead of calling their hooks. A hook delivers events and marks a pane
    /// dirty, and a session's feeder thread that reads those deliveries may itself be waiting for a
    /// lock this caller holds — so the caller runs [`Self::wake_owners`] once it has let go.
    pub fn collect_caps_reclaim(&self, owner: u64) -> Vec<u64> {
        // Declared before the lock guard so everything it collects is freed after the mutex goes.
        let mut freed = Freed::default();
        let mut inner = self.inner.lock();
        let (_, queued_job) = Self::enforce_inner(&mut inner, owner, Reason::Routine, &mut freed);
        drop(inner);
        if queued_job {
            // A refusal for an evicted owed key sits on the queue: do not let it wait for some
            // other submit to happen to wake the worker.
            self.wake.notify_one();
        }
        freed.wake
    }

    /// Ask each owner in `owners` (duplicates collapsed) to redraw, through the hook its session
    /// registered. Called after the store mutex is released: a hook delivers events and marks a
    /// pane dirty, and nothing of that belongs under the lock — the rule the worker already keeps.
    /// A caller that held a lock of its own wakes only after dropping that too (finding M3).
    pub fn wake_owners(&self, owners: &[u64]) {
        let mut seen: Vec<u64> = Vec::with_capacity(owners.len());
        for owner in owners {
            if !seen.contains(owner) {
                seen.push(*owner);
            }
        }
        for owner in seen {
            // One short lock per owner to read its hook out; the call itself happens with nothing
            // held. An owner that vanished meanwhile (its terminal dropped) needs no redraw.
            let wake = {
                let inner = self.inner.lock();
                inner
                    .owners
                    .get(&owner)
                    .and_then(|e| e.hooks.as_ref().map(|h| h.wake.clone()))
            };
            if let Some(wake) = wake {
                wake();
            }
        }
    }

    /// The visible-state epoch of one owner's pictures: bumped under the lock each time an eviction
    /// or a refusal changes what that owner can draw (see [`Freed::changed`]), and never by a lookup,
    /// a `touch_drawn` or a satisfied install. The widget puts it in its frame-cache key so a woken
    /// pane rebuilds instead of repainting the stale frame (the reason it exists). An owner with no
    /// entry — never seen, or dropped — reads 0.
    pub fn generation(&self, owner: u64) -> u64 {
        let inner = self.inner.lock();
        inner.owners.get(&owner).map_or(0, |entry| entry.generation)
    }

    /// Stamp `keys` as shown for `owner`, one lock for the whole list.
    ///
    /// The draw path normally stamps through [`Self::get`]; a frame that was reused without asking
    /// (nothing about the pane changed) still shows the same pictures, and without this those
    /// pictures would age out of the LRU while plainly on screen. Keys the owner no longer has are
    /// skipped; nothing is queued and nothing is woken — this only moves stamps.
    pub fn touch_drawn(&self, owner: u64, keys: &[ImageKey]) {
        if keys.is_empty() {
            return;
        }
        let mut inner = self.inner.lock();
        let Some(entry) = inner.owners.get_mut(&owner) else {
            return;
        };
        for key in keys {
            if let Some(image) = entry.images.get_mut(key) {
                image.shown = self.next_stamp();
            }
        }
    }

    /// Decoded bytes, payloads and resamples the store holds across all owners.
    pub fn bytes_held(&self) -> usize {
        self.inner.lock().total
    }

    /// The same for one owner; `0` when it has nothing (or never had anything).
    pub fn owner_bytes(&self, owner: u64) -> usize {
        self.inner.lock().owners.get(&owner).map_or(0, |o| o.bytes)
    }

    /// Run queued jobs on the calling thread until none are left. The app's worker does this on
    /// its own thread; tests drive their own store with it (it is also what a caller without a
    /// thread would use — never call it while holding the terminal lock, jobs lock terminals).
    pub fn run_jobs_until_idle(&self) {
        loop {
            let job = self.inner.lock().jobs.pop_front();
            let Some(job) = job else { return };
            // Same containment as the worker thread: a job must not strand its caller.
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.run_job(job))).is_err()
            {
                tracing::error!("an image job panicked on the driving thread; the queue continues");
            }
        }
    }

    /// Block until no job is queued and none runs. Returns at once when the thread never started.
    pub fn wait_idle(&self) {
        let mut inner = self.inner.lock();
        while !inner.jobs.is_empty() || inner.busy {
            self.idle.wait(&mut inner);
        }
    }

    /// [`Self::wait_idle`] for at most `timeout`; returns whether the store was idle.
    pub fn wait_idle_for(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut inner = self.inner.lock();
        while !inner.jobs.is_empty() || inner.busy {
            if self.idle.wait_until(&mut inner, deadline).timed_out() {
                return inner.jobs.is_empty() && !inner.busy;
            }
        }
        true
    }

    fn next_stamp(&self) -> u64 {
        self.stamper.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn over(inner: &Inner, owner: u64) -> bool {
        if inner.total > inner.total_cap {
            return true;
        }
        inner
            .owners
            .get(&owner)
            .is_some_and(|o| o.bytes > inner.per_owner_cap)
    }

    /// Evict until `owner`'s caps hold, in the order [`Self::enforce_caps`] documents. Under
    /// [`Reason::Install`] the named whole image and that one copy are skipped (a resample result
    /// must not be evicted by the very enforcement its install triggers) and every other copy
    /// this pass drops is marked refused (see [`Slot::Refused`]); [`Reason::Routine`] is plain
    /// LRU. Returns whether the caps were met (`false`: only the protected image is left keeping
    /// them over, and the caller must undo its install — see [`Self::finish_resample`]) and
    /// whether a job was queued (a refusal for an evicted owed key; the caller wakes the worker).
    fn enforce_inner(
        inner: &mut Inner,
        owner: u64,
        reason: Reason,
        freed: &mut Freed,
    ) -> (bool, bool) {
        let mut queued_job = false;
        while Self::over(inner, owner) {
            let dropped = evict_resample(inner, owner, Scope::Own, reason, freed)
                || evict_image(inner, owner, Scope::Own, reason, freed, &mut queued_job)
                || evict_resample(inner, owner, Scope::Others, reason, freed)
                || evict_image(inner, owner, Scope::Others, reason, freed, &mut queued_job);
            if !dropped {
                // Nothing outside the protected image can go; never loop on a cap that cannot be met.
                return (false, queued_job);
            }
        }
        (true, queued_job)
    }

    /// Notify idle waiters when nothing is left (called with the lock held).
    fn check_idle(inner: &Inner, idle: &Condvar) {
        if inner.jobs.is_empty() && !inner.busy {
            idle.notify_all();
        }
    }

    /// Start this store's worker thread if it is the process-wide one and has work. Instances a
    /// test built drive their jobs through [`ImageStore::run_jobs_until_idle`] instead.
    fn start_global_worker(&self) {
        if let Some(global) = STORE.get()
            && std::ptr::eq(self, global)
        {
            global.start();
        }
    }

    /// Start the worker thread unless it runs (only meaningful for the process-wide store, which
    /// lives long enough to be `'static`). A panic that escapes everything restarts the thread at
    /// once if work is left.
    pub fn start(&'static self) {
        if self.running.load(Ordering::Relaxed) || self.running.swap(true, Ordering::SeqCst) {
            return;
        }
        let spawned = std::thread::Builder::new()
            .name("fbterm-image-worker".into())
            .spawn(move || {
                let _exit = ThreadExit(self);
                self.run();
            });
        if spawned.is_err() {
            static WARNED: AtomicBool = AtomicBool::new(false);
            if !WARNED.swap(true, Ordering::Relaxed) {
                tracing::warn!("cannot start the image worker; images wait for a later submit");
            }
            self.running.store(false, Ordering::SeqCst);
        }
    }

    /// The worker thread's loop: returns only by unwinding (see [`ThreadExit`]).
    fn run(&self) -> ! {
        loop {
            let job = {
                let mut inner = self.inner.lock();
                loop {
                    if let Some(job) = inner.jobs.pop_front() {
                        inner.busy = true;
                        break job;
                    }
                    self.wake.wait(&mut inner);
                }
            };
            // A job must not take the thread (and every later image) down: panics inside a
            // decoder are already folded into `Corrupt` by `decode_with`; this catches anything
            // else, such as a session hook that panicked while answering. Clearing `busy` and
            // waking idle waiters happens afterwards whatever happened.
            let done = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.run_job(job)));
            let mut inner = self.inner.lock();
            inner.busy = false;
            Self::check_idle(&inner, &self.idle);
            if done.is_err() {
                tracing::error!(
                    "the image worker caught a panic finishing a job; the queue continues"
                );
            }
        }
    }

    fn run_job(&self, job: Job) {
        match job {
            Job::Decode {
                owner,
                key,
                serial,
                data,
            } => {
                let hook = self.decode_hook.lock().clone();
                let result = decode::decode_with(&data, hook.as_ref());
                self.finish_decode(owner, key, serial, result);
            }
            Job::Resample {
                owner,
                key,
                target,
                stretch,
                integer,
                src,
            } => {
                let source = (src.width, src.height);
                // The picture's own size asked back: nothing to filter and no second frame to make.
                // Clone the handle over the source's shared bytes (a refcount bump) and hand that
                // over as the copy (finding P4).
                let copy = if target == source {
                    Copy::Shares(Handle::from_rgba(src.width, src.height, src.pixels.clone()))
                } else {
                    Copy::Made(resample(&src, target, integer))
                };
                self.finish_resample(owner, key, target, stretch, copy, source);
            }
            Job::Refuse { owner, key, serial } => self.finish_refusal(owner, key, serial),
            Job::Answer { owner, key, serial } => self.finish_answer(owner, key, serial),
        }
    }

    /// Install a decode result and answer for its key. Lock order: store first (short), released
    /// before the terminal lock; hooks run with no lock held at all.
    fn finish_decode(
        &self,
        owner: u64,
        key: ImageKey,
        serial: u64,
        result: Result<Rgba8Image, DecodeError>,
    ) {
        // Which answer, if any, this job still owns (see the exactly-once rule on [`Job`]).
        enum Claim {
            /// Still this job's pending decode: install the result and answer with it.
            Decode(usize),
            /// The caps evicted the entry while the decode ran: answer `TooLarge`, drop the buffer.
            Refusal,
        }
        // (bytes released, bytes installed): declared outside the block so a hookless install can
        // still enforce the caps afterwards (see below).
        let mut delta = (0usize, 0usize);
        let delivery = {
            let mut inner = self.inner.lock();
            let claim = inner
                .owners
                .get(&owner)
                .and_then(|entry| entry.images.get(&key))
                .filter(|image| image.serial == serial && image.owes_reply)
                .map(|image| match image.state {
                    State::Pending(payload) => Claim::Decode(payload),
                    _ => Claim::Refusal,
                });
            let Some(claim) = claim else {
                return; // released, replaced, or a `Refuse` answered it first: nothing to do
            };
            let answer: Result<(), ImageDecodeError> = match &claim {
                Claim::Decode(_) => match &result {
                    Ok(_) => Ok(()),
                    Err(e) => Err(ImageDecodeError::from(*e)),
                },
                Claim::Refusal => Err(ImageDecodeError::TooLarge),
            };
            let decoded = match &claim {
                Claim::Decode(_) => result.as_ref().map_or(0, |img| img.bytes()),
                Claim::Refusal => 0,
            };
            // The result is dropped unused on a refusal: the payload's bytes went when it was
            // evicted, and this buffer has no place left to live in.
            let hooks = inner.owners.get_mut(&owner).map(|entry| {
                if let Some(image) = entry.images.get_mut(&key)
                    && image.serial == serial
                    && image.owes_reply
                {
                    // Claimed only when the answer can actually be delivered now; with no hooks
                    // it waits here and a later `register_owner` queues its delivery (Job::Answer).
                    if entry.hooks.is_some() {
                        image.owes_reply = false; // claimed: no `Refuse` job will answer this key
                    } else {
                        image.pending_answer.get_or_insert(answer);
                        tracing::debug!(
                            owner,
                            "no hooks for an image owner yet; its decode answer waits for a registration"
                        );
                    }
                    delta = match claim {
                        Claim::Decode(payload) => {
                            image.state = match result {
                                Ok(img) => {
                                    // What the container actually holds, which for a GIF frame can
                                    // be smaller than the logical screen its header declared.
                                    image.source = (img.width, img.height);
                                    State::Ready(Arc::new(img))
                                }
                                // A corrupt or oversized payload: the key stays known as evicted
                                // until its release arrives, so draws answer `Evicted`.
                                Err(_) => State::Evicted,
                            };
                            (payload, decoded)
                        }
                        Claim::Refusal => (0, 0),
                    };
                    entry.bytes = entry.bytes - delta.0 + delta.1;
                }
                entry
                    .hooks
                    .as_ref()
                    .map(|h| (h.term.clone(), h.deliver.clone(), h.wake.clone()))
            });
            inner.total = inner.total - delta.0 + delta.1;
            hooks.flatten().map(|hooks| (answer, hooks))
        };
        let Some((answer, hooks)) = delivery else {
            // Nothing could be delivered — no hooks yet (the answer waits for a registration) or
            // the entry is gone. A buffer installed anyway still counts against the caps, and
            // nobody else runs enforcement before the next submit, so do it here. Scrollback
            // reclaim needs the terminal; this stays plain cap enforcement.
            if delta.1 > 0 {
                self.enforce_caps(owner);
            }
            return;
        };
        self.deliver(owner, key, answer, hooks);
    }

    /// Answer a key the caps evicted before its decode could run: `Err(TooLarge)`, through the
    /// same path a decode result takes. The client gets its `ENOMEM` and the emulator frees the
    /// pending-decode slot and placements it was holding for the image.
    fn finish_refusal(&self, owner: u64, key: ImageKey, serial: u64) {
        let hooks =
            {
                let mut inner = self.inner.lock();
                inner.owners.get_mut(&owner).and_then(|entry| {
                let image = entry.images.get_mut(&key)?;
                if image.serial != serial || !image.owes_reply {
                    // Its running decode answered first, or the key is gone: nothing waits.
                    return None;
                }
                let hooks = entry
                    .hooks
                    .as_ref()
                    .map(|h| (h.term.clone(), h.deliver.clone(), h.wake.clone()));
                if hooks.is_some() {
                    image.owes_reply = false; // claimed: the decode result will not answer
                    return hooks;
                }
                // Nobody to answer yet: the refusal waits here for a registration (Job::Answer).
                image.pending_answer.get_or_insert(Err(ImageDecodeError::TooLarge));
                tracing::debug!(
                    owner,
                    "no hooks for an image owner yet; its eviction answer waits for a registration"
                );
                None
            })
            };
        let Some(hooks) = hooks else { return };
        self.deliver(owner, key, Err(ImageDecodeError::TooLarge), hooks);
    }

    /// Deliver an answer that was stashed while its owner had no hooks (see [`Job::Answer`]).
    /// Claims under the lock — serial match, still owed, an answer actually waiting — so it races
    /// safely with a `Refuse` or a re-submit; then hands off through the same delivery path as
    /// any other answer. If the owner is unregistered again in the meantime the answer stays put
    /// and the next registration queues the job anew.
    fn finish_answer(&self, owner: u64, key: ImageKey, serial: u64) {
        let delivery = {
            let mut inner = self.inner.lock();
            inner.owners.get_mut(&owner).and_then(|entry| {
                let hooks = entry
                    .hooks
                    .as_ref()
                    .map(|h| (h.term.clone(), h.deliver.clone(), h.wake.clone()));
                let hooks = hooks?; // unregistered again: the answer waits for the next one
                let image = entry.images.get_mut(&key)?;
                if image.serial != serial || !image.owes_reply {
                    return None; // answered another way, or the key is gone
                }
                let answer = image.pending_answer.take()?;
                image.owes_reply = false; // claimed: delivered now (see the rule on [`Job`])
                Some((answer, hooks))
            })
        };
        let Some((answer, hooks)) = delivery else {
            return;
        };
        self.deliver(owner, key, answer, hooks);
    }

    /// Hand one decode answer to the terminal and its session. No store lock is held here: the
    /// caps are reclaimed first (scrollback first, exactly as after a submit — see [`reclaim`]),
    /// then the terminal is locked briefly to answer — leaving what that answer raised in the
    /// terminal's own event queue — then the hooks run with nothing held. `deliver` is what takes
    /// those events out: an answer queued here cannot be carried past a reply that output produced
    /// before it, because only whoever routes empties the queue. A dropped terminal means nobody
    /// waits, so it gets nothing.
    fn deliver(
        &self,
        owner: u64,
        key: ImageKey,
        answer: Result<(), ImageDecodeError>,
        hooks: DeliveryHooks,
    ) {
        let (term, deliver, wake) = hooks;
        let changed = reclaim(self, &term, owner);
        // Nothing is held here, so the owners this reclaim displaced — possibly other panes — are
        // asked to redraw right away, before anything of this answer goes out.
        self.wake_owners(&changed);
        let Some(term) = term.upgrade() else { return };
        {
            let mut guard = term.lock();
            // Answers the client that was waiting (`OK`, `EBADPNG` or `ENOMEM`) and, on failure,
            // removes the image and its placements (raising their releases).
            guard.image_decoded(key, answer);
        }
        drop(term);
        deliver();
        wake();
    }

    /// Install a finished resample — or refuse it — and wake the pane either way.
    ///
    /// The pixel buffer becomes an iced image handle here, on the worker, once: that is what makes
    /// "an image is uploaded to the GPU once" (spec goal 4) true, because every later frame draws
    /// the same handle id and iced reuses its texture. `source` is the source image's pixel size,
    /// which the drawer needs to map a placement's `src` rectangle onto this copy. A refusal wakes
    /// too: the frame that asked is sitting on `pending`, and only asking again answers the
    /// placeholder spec 2.6 and 6.3 draw in the empty box (the refused slot then answers without
    /// queueing, so this cannot become a loop).
    fn finish_resample(
        &self,
        owner: u64,
        key: ImageKey,
        target: (u32, u32),
        stretch: bool,
        copy: Copy,
        source: (u32, u32),
    ) {
        // Declared before the lock guard so everything enforcement takes goes after the mutex.
        let mut freed = Freed::default();
        let slot_key = resample_key(target, stretch);
        {
            let mut inner = self.inner.lock();
            // Only install onto the exact queue slot this job filled: if the source was evicted
            // or released meanwhile, its Arc in this job is what kept it alive, and now goes.
            let queued = inner
                .owners
                .get(&owner)
                .and_then(|e| e.images.get(&key))
                .is_some_and(|image| {
                    matches!(image.state, State::Ready(_))
                        && matches!(image.resamples.get(&slot_key), Some(Slot::Queued))
                });
            if !queued {
                return;
            }
            let out = Arc::new(match copy {
                Copy::Made(img) => ResampledImage {
                    width: img.width,
                    height: img.height,
                    source,
                    // The one copy of these pixels iced ever sees: `from_rgba` takes the buffer by
                    // value, so nothing is copied here and no codec runs — the store decoded it.
                    handle: Handle::from_rgba(img.width, img.height, img.pixels),
                },
                Copy::Shares(handle) => ResampledImage {
                    width: source.0,
                    height: source.1,
                    source,
                    // The source's own pixels under a handle of their own: the same buffer, and no
                    // frame made for it (finding P4).
                    handle,
                },
            });
            let stamp = self.next_stamp();
            let bytes = out.charge();
            // A copy that cannot fit beside its own source under the tightest cap is refused up
            // front, as a GEOMETRY refusal: no enforcement pass could free more than "everything
            // except this pair", so nothing that frees other memory can change this, and evicting
            // other images for a copy that comes back out anyway is pure collateral. An alias of
            // the source accounts nothing, so it always fits (finding P4).
            let cannot_fit = !out.shares_source()
                && inner
                    .owners
                    .get(&owner)
                    .and_then(|e| e.images.get(&key))
                    .is_some_and(|image| {
                        let State::Ready(src) = &image.state else {
                            return false; // vanished with the queued slot: nothing to install onto
                        };
                        src.bytes() + bytes > inner.per_owner_cap.min(inner.total_cap)
                    });
            if cannot_fit {
                if let Some(image) = inner
                    .owners
                    .get_mut(&owner)
                    .and_then(|e| e.images.get_mut(&key))
                {
                    image
                        .resamples
                        .insert(slot_key, Slot::Refused(Refusal::Geometry));
                }
                freed.copies.push(out); // the made buffer goes after the mutex, untouched
                // Refused IS an answer — the drawer draws the placeholder for it — and the frame
                // that asked is parked on `pending` until it gets one, so this owner must be asked
                // to redraw. The refused slot answers without queueing afterwards: one wake ends it.
                // (Here even a bare re-ask would answer differently than the asking frame drew, so
                // the generation moves with the wake — see [`Freed::changed`].)
                freed.changed(&mut inner, owner);
            } else if out.shares_source() {
                // The picture's own pixels, accounted where they already are: install the alias and
                // wake the frame that parked on `pending` waiting for it. There is nothing for an
                // enforcement pass to win here — this copy adds no bytes (finding P4) — so none runs.
                if let Some(entry) = inner.owners.get_mut(&owner)
                    && let Some(image) = entry.images.get_mut(&key)
                {
                    image.resamples.insert(slot_key, Slot::Ready(stamp, out));
                }
                freed.woke(owner);
            } else {
                if let Some(entry) = inner.owners.get_mut(&owner)
                    && let Some(image) = entry.images.get_mut(&key)
                {
                    image.resamples.insert(slot_key, Slot::Ready(stamp, out));
                    image.resample_bytes += bytes;
                    entry.bytes += bytes;
                }
                inner.total += bytes;
                // Resampled bytes count against the caps too, so they are enforced here — with this
                // copy and its source image protected: what goes first is an older copy or an older
                // picture, never the one just asked for. Every other copy this drops is marked
                // refused under pressure (see [`Reason::Install`]) and wakes its owner
                // (see [`Freed::wake`]): a displaced on-screen size becomes its placeholder instead
                // of resampling once per frame beside the new copy, and a pane whose picture went
                // must see that. If only the protected picture is left and a cap is still over —
                // unreachable with the up-front check, which would have refused it before evicting
                // anything for it — the copy comes back out and the size is refused all the same,
                // as a GEOMETRY refusal: with only its own source left, nothing can make room.
                // A refusal enforcement queues for an evicted owed key needs no wake of its own:
                // this job's worker loop takes whatever is next.
                let (satisfied, _queued) = Self::enforce_inner(
                    &mut inner,
                    owner,
                    Reason::Install(Protect {
                        owner,
                        key,
                        slot: slot_key,
                    }),
                    &mut freed,
                );
                if satisfied {
                    freed.woke(owner); // the new copy is there: draw it
                } else {
                    let mut refused_bytes = 0usize;
                    if let Some(entry) = inner.owners.get_mut(&owner)
                        && let Some(image) = entry.images.get_mut(&key)
                        && let Some(Slot::Ready(_, copy)) = image.resamples.remove(&slot_key)
                    {
                        let gone = copy.bytes();
                        entry.bytes -= gone;
                        image.resample_bytes -= gone;
                        freed.copies.push(copy);
                        // Only its own source is left and the cap is still over: geometry,
                        // so this size stays refused until the source changes.
                        image
                            .resamples
                            .insert(slot_key, Slot::Refused(Refusal::Geometry));
                        refused_bytes = gone;
                    }
                    inner.total -= refused_bytes;
                    if refused_bytes > 0 {
                        // The size came back out as `Refused`: a visible change, so its generation
                        // moves with the wake (see [`Freed::changed`]).
                        freed.changed(&mut inner, owner);
                    } else {
                        freed.woke(owner);
                    }
                }
            }
        }
        self.wake_owners(&freed.wake);
    }
}

/// Reclaim one owner's image memory in the order spec 6.3 lays down: its terminal's scrollback
/// first — each evicted placement becomes a placeholder line and releases its image outright,
/// which is handed back to the store — and only when that runs dry do the store's own buffers go.
///
/// Lock order: call this with neither the store mutex nor a terminal lock held. It locks one
/// terminal at a time and lets go before touching the store, so it keeps the rule that the store
/// never locks a terminal under its own mutex. Both the session router (after a submit) and the
/// image worker (when a decode or a refusal finishes) call it, which is what makes "scrollback
/// first" hold however the bytes grew. The terminal also emits `ImageReleased` for each eviction;
/// [`ImageStore::release`] absorbs the duplicate.
///
/// Nothing is woken here: the owners whose visible state changed are returned instead, and the
/// caller runs [`ImageStore::wake_owners`] once it holds no lock of its own. A wake delivers events
/// and marks a pane dirty, and the session thread that reads those deliveries can be waiting for
/// the very lock the caller holds (finding M3).
pub fn reclaim(store: &ImageStore, term: &Weak<Mutex<Terminal>>, owner: u64) -> Vec<u64> {
    while store.over_cap(owner) {
        let Some(term) = term.upgrade() else {
            break; // the pane is gone; its whole share goes with it in `drop_owner`
        };
        let evicted = term.lock().evict_oldest_scrollback_image();
        drop(term);
        let Some(key) = evicted else {
            break; // scrollback holds no image left to reclaim
        };
        store.release(owner, key);
    }
    store.collect_caps_reclaim(owner)
}

/// Clears the worker's `running` flag when its thread ends (only by a panic escaping everything),
/// and starts a new one if work is still queued so nothing waits for a later submit.
struct ThreadExit(&'static ImageStore);

impl Drop for ThreadExit {
    fn drop(&mut self) {
        tracing::error!("the image worker stopped; it restarts for queued work");
        self.0.running.store(false, Ordering::SeqCst);
        if !self.0.inner.lock().jobs.is_empty() {
            self.0.start();
        }
    }
}

fn job_owner(job: &Job) -> u64 {
    match job {
        Job::Decode { owner, .. }
        | Job::Resample { owner, .. }
        | Job::Refuse { owner, .. }
        | Job::Answer { owner, .. } => *owner,
    }
}

fn job_is(job: &Job, owner: u64, key: ImageKey) -> bool {
    match job {
        Job::Decode {
            owner: o, key: k, ..
        }
        | Job::Resample {
            owner: o, key: k, ..
        }
        | Job::Refuse {
            owner: o, key: k, ..
        }
        | Job::Answer {
            owner: o, key: k, ..
        } => *o == owner && *k == key,
    }
}

/// The area of the source image a placement shows: its `src` rectangle clamped to the image, or
/// the whole image with no `src`. A zero `w` or `h` means "to the end" and the displayed area is
/// the intersection with the image (feathercritty's [`Rect`] docs), so a rectangle that starts
/// past the edge still yields at least one pixel rather than zero — a zero area would mean a
/// divide by zero in [`fit_target`], and a placement cannot display less than a pixel.
///
/// `pub(crate)` because the drawer measures a picture the same way before asking for it: the box it
/// asks for is scaled by this area, not by the whole picture (finding C2), and asking for exactly
/// this area is what draws a picture 1:1 (finding I1).
pub(crate) fn src_area(src: Option<Rect>, img_w: u32, img_h: u32) -> (u32, u32) {
    let Some(rect) = src else {
        return (img_w.max(1), img_h.max(1));
    };
    let axis = |off: u32, len: u32, side: u32| -> u32 {
        let start = off.min(side.saturating_sub(1));
        let room = side.saturating_sub(start);
        if len == 0 {
            room.max(1)
        } else {
            len.min(room).max(1)
        }
    };
    (axis(rect.x, rect.w, img_w), axis(rect.y, rect.h, img_h))
}

/// The size a placement's resample is made at: the WHOLE image scaled by the factor that fits its
/// shown area inside `box_px`, rounded to whole pixels and then capped by [`cap_area`]. Cropping
/// happens in the drawer (see [`crate::widget::images::crop_for_src`]), which keeps one copy per
/// size instead of one per rectangle.
fn fit_target(whole: &Rgba8Image, src: Option<Rect>, box_px: (u32, u32)) -> (u32, u32) {
    let (area_w, area_h) = src_area(src, whole.width, whole.height);
    let factor = (f64::from(box_px.0.max(1)) / f64::from(area_w))
        .min(f64::from(box_px.1.max(1)) / f64::from(area_h));
    // `as u32` on a float saturates, and the cap below shrinks whatever survives; a hostile box
    // cannot make this wrap.
    let w = (f64::from(whole.width) * factor)
        .round()
        .max(1.0)
        .min(u64::from(u32::MAX) as f64) as u32;
    let h = (f64::from(whole.height) * factor)
        .round()
        .max(1.0)
        .min(u64::from(u32::MAX) as f64) as u32;
    cap_area(w, h)
}

/// The size a stretched placement's resample is made at: the span's own physical box, capped by
/// [`cap_area`] exactly as a fitted size is so a hostile span cannot ask for a buffer over the
/// per-image byte cap. The picture's ratio is ignored here, which is the whole of what a protocol
/// asks for when it says to stretch; nothing else does.
fn fill_target(box_px: (u32, u32)) -> (u32, u32) {
    cap_area(box_px.0.max(1), box_px.1.max(1))
}

/// The store's further bound on a fitted size: never more than one
/// [`MAX_IMAGE_BYTES`](feathercritty::graphics::limits::MAX_IMAGE_BYTES) RGBA8 frame, so even an
/// absurd placement request cannot ask for a buffer bigger than the per-image cap.
fn cap_area(mut w: u32, mut h: u32) -> (u32, u32) {
    use feathercritty::graphics::limits::MAX_IMAGE_BYTES;
    // Pixels-per-axis bound written as an area comparison: `w * h` of two u32s always fits a u64,
    // multiplying by 4 first would not. Shrinking keeps the aspect (floors) and always progresses.
    while (w as u64) * (h as u64) > MAX_IMAGE_BYTES / 4 {
        let scale = (((MAX_IMAGE_BYTES / 4) as f64) / (f64::from(w) * f64::from(h))).sqrt();
        let next_w = ((f64::from(w) * scale).floor() as u32).max(1);
        let next_h = ((f64::from(h) * scale).floor() as u32).max(1);
        if next_w == w && next_h == h {
            // Rounding stuck (a sliver that floors to itself): step the longer axis down.
            if w >= h {
                w = (w - 1).max(1);
            } else {
                h = (h - 1).max(1);
            }
        } else {
            w = next_w;
            h = next_h;
        }
    }
    (w, h)
}

/// Which owners an eviction step may take from: the one that triggered enforcement, or the rest.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    Own,
    Others,
}

/// One candidate install-time enforcement must skip: `key`'s whole image for `owner`, and its
/// copy at exactly `slot` (the other copies of the same picture stay evictable — they are what a
/// moved drawer no longer needs; see [`ImageStore::enforce_inner`]).
#[derive(Clone, Copy)]
struct Protect {
    owner: u64,
    key: ImageKey,
    slot: ResampleKey,
}

/// Why an enforcement pass runs, and what it is allowed to do about copies.
///
/// [`Reason::Routine`] is reclaim after a submit or a decode completion: plain LRU, and evicted
/// copies stay re-makeable — the drawer may ask for them again and get them.
/// [`Reason::Install`] names the copy being put in place: that copy and its source are skipped,
/// and every *other* copy this pass drops is marked [`Slot::Refused(Refusal::Pressure)`]. A copy
/// of a still-held image displaced by an install must become its placeholder rather than resample
/// once per frame — otherwise two placements whose copies cannot coexist evict each other forever
/// (the loop the round-3 review calls N4). The refusal is a pressure refusal: it keeps the loop
/// dead only until memory frees, so a release or dropped owner that actually removed something
/// clears it, and the size is asked afresh on the next draw (see
/// [`Freed::clear_pressure_refusals`]).
#[derive(Clone, Copy)]
enum Reason {
    Routine,
    Install(Protect),
}

/// Whether `owner` is in an eviction scope for the owner that triggered it.
fn covers(owner: u64, trigger: u64, scope: Scope) -> bool {
    match scope {
        Scope::Own => owner == trigger,
        Scope::Others => owner != trigger,
    }
}

/// Drop the least recently shown resampled copy in scope. The scan skips the copy `Reason::Install`
/// names (with [`Reason::Routine`] — and for whole-image candidates, always — this is exactly the
/// plain LRU step), and it skips a copy that is its picture's own pixels: an alias accounts nothing
/// (finding P4), so dropping it would free nothing and leave the cap where it was. An install-time
/// eviction marks the losing size [`Slot::Refused(Refusal::Pressure)`]: a copy displaced to make
/// room for a fresh install must not be re-made every frame — but the pressure that refused it
/// ends when memory frees, so the size is asked afresh on the next draw after the next release or
/// dropped owner that actually removed something (`ImageStore::release` and
/// `ImageStore::drop_owner`). A plain LRU eviction leaves the slot empty and re-makeable.
/// Returns whether anything went.
fn evict_resample(
    inner: &mut Inner,
    trigger: u64,
    scope: Scope,
    reason: Reason,
    freed: &mut Freed,
) -> bool {
    let mut best: Option<(u64, u64, ImageKey, ResampleKey)> = None; // (stamp, owner, key, slot)
    for (owner, entry) in &inner.owners {
        if !covers(*owner, trigger, scope) {
            continue;
        }
        for (key, image) in &entry.images {
            for (slot_key, state) in &image.resamples {
                let Slot::Ready(stamp, out) = state else {
                    continue;
                };
                if out.shares_source() {
                    continue; // the picture's own pixels: taking them back frees nothing
                }
                if matches!(reason, Reason::Install(p)
                    if p.owner == *owner && p.key == *key && p.slot == *slot_key)
                {
                    continue; // the copy enforcement was just asked to keep
                }
                if best.as_ref().is_none_or(|(s, ..)| *stamp < *s) {
                    best = Some((*stamp, *owner, *key, *slot_key));
                }
            }
        }
    }
    let Some((_, owner, key, slot_key)) = best else {
        return false;
    };
    // The scan borrowed; re-look the winner up mutably (it returned plain values).
    let entry = inner.owners.get_mut(&owner).unwrap();
    let image = entry.images.get_mut(&key).unwrap();
    if let Some(Slot::Ready(_, out)) = image.resamples.remove(&slot_key) {
        // Never an alias (the scan skipped those), so this really does free a frame.
        let bytes = out.charge();
        entry.bytes -= bytes;
        image.resample_bytes -= bytes;
        freed.copies.push(out); // dropped after the mutex goes
        if matches!(reason, Reason::Install(..)) {
            // The source is still held (only copies are evicted here), so this size draws its
            // placeholder — refusing it under pressure stops the ask-evict-ask loop. The refusal
            // is a [`Refusal::Pressure`] one: when memory frees, the next release or dropped
            // owner clears it, and the size is asked afresh on the next draw.
            image
                .resamples
                .insert(slot_key, Slot::Refused(Refusal::Pressure));
        }
        inner.total -= bytes;
    }
    // Whatever was on screen for this owner just changed (and under `Reason::Install` a size was
    // refused as well): its cached frame holds a handle for pixels that are gone, and only asking
    // again answers what to draw in their place — so the generation bumps too (see [`Freed::changed`]).
    freed.changed(inner, owner);
    true
}

/// Drop the least recently shown whole image in scope: its buffer and every resampled copy of it
/// (the source is gone — any refused sizes, of either kind, go with it, so they are asked afresh
/// if the key is re-submitted: the whole entry, refusals included, is drained). A pending decode
/// counts as a whole image too, and evicting one must not strand its answer: its queued jobs are
/// purged and a [`Job::Refuse`] takes their place (if the decode was already running, both exist
/// and the exactly-once rule on [`Job`] picks one) — `queued_job` reports that push so the caller
/// can wake the worker. The image `Reason::Install` names is skipped in the scan.
fn evict_image(
    inner: &mut Inner,
    trigger: u64,
    scope: Scope,
    reason: Reason,
    freed: &mut Freed,
    queued_job: &mut bool,
) -> bool {
    let mut best: Option<(u64, u64, ImageKey)> = None; // (shown, owner, key)
    for (owner, entry) in &inner.owners {
        if !covers(*owner, trigger, scope) {
            continue;
        }
        for (key, image) in &entry.images {
            if matches!(image.state, State::Evicted) {
                continue;
            }
            if matches!(reason, Reason::Install(p) if p.owner == *owner && p.key == *key) {
                continue; // the whole image enforcement was just asked to keep
            }
            if best.as_ref().is_none_or(|(s, ..)| image.shown < *s) {
                best = Some((image.shown, *owner, *key));
            }
        }
    }
    let Some((_, owner, key)) = best else {
        return false;
    };
    let entry = inner.owners.get_mut(&owner).unwrap();
    let image = entry.images.get_mut(&key).unwrap();
    let gone = image.accounted();
    let owed = image.owes_reply;
    let serial = image.serial;
    entry.bytes -= gone;
    inner.total -= gone;
    if let State::Ready(img) = std::mem::replace(&mut image.state, State::Evicted) {
        freed.images.push(img);
    }
    freed.drain_resamples(std::mem::take(&mut image.resamples));
    image.resample_bytes = 0;
    // Purge queued work for this key so its payload and source copies go now, not at completion.
    Freed::take_jobs(inner, |job| !job_is(job, owner, key), freed);
    if owed {
        inner.jobs.push_back(Job::Refuse { owner, key, serial });
        *queued_job = true;
    }
    // The picture itself went: its placements draw placeholders now, so this owner's cached frame
    // must be rebuilt and its generation bumped (see [`Freed::changed`]).
    freed.changed(inner, owner);
    true
}

static STORE: OnceLock<ImageStore> = OnceLock::new();

/// The process-wide store: 256 MiB per terminal, 512 MiB in all. Its worker thread starts with
/// the first job; calling this function alone allocates a (tiny) empty store and no thread.
pub fn store() -> &'static ImageStore {
    STORE.get_or_init(|| ImageStore::with_caps(PER_OWNER_CAP, TOTAL_CAP))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GridSize;
    use crate::images::decode::fixtures::{TINY_PNG, tiny_rgba};
    use feathercritty::event::ImageFormat;
    use std::sync::Mutex as StdMutex;

    const KB: usize = 1024;

    fn size() -> GridSize {
        GridSize {
            cols: 80,
            rows: 24,
            cell_width: 8.0,
            cell_height: 16.0,
            scale: 1.0,
        }
    }

    /// A flat `w x h` RGBA picture of one value — byte counts are exact in cap tests.
    fn rgba(w: u32, h: u32, v: u8) -> ImageData {
        ImageData::Rgba {
            width: w,
            height: h,
            pixels: vec![v; (w as usize) * (h as usize) * 4],
        }
    }

    fn png(bytes: &[u8]) -> ImageData {
        ImageData::Encoded {
            format: ImageFormat::Png,
            bytes: bytes.to_vec(),
            width: 4,
            height: 4,
        }
    }

    /// The books must balance after every store operation: the app-wide total equals the sum of
    /// the owners' byte counts, and each owner's count equals the sum of its entries' whole-image
    /// (or pending-payload) bytes plus their resampled copies. A path that installs or drops a
    /// buffer without adjusting one of these ledgers shows up here, not as a silent leak.
    fn assert_books(store: &ImageStore) {
        let inner = store.inner.lock();
        let mut sum = 0usize;
        for (owner, entry) in &inner.owners {
            let owner_sum: usize = entry.images.values().map(Entry::accounted).sum();
            assert_eq!(
                entry.bytes, owner_sum,
                "owner {owner}'s byte count is wrong"
            );
            sum += owner_sum;
        }
        assert_eq!(
            inner.total, sum,
            "the app-wide total does not match the owners"
        );
    }

    /// Counts one hook call that ran while the probed store's mutex was still held. `try_lock`
    /// returns a guard only when it actually takes the mutex; binding it in a statement drops it
    /// again immediately, so this cannot deadlock.
    fn record_held(held: &std::sync::atomic::AtomicUsize, store: Option<&ImageStore>) {
        let Some(store) = store else { return };
        if store.inner.try_lock().is_none() {
            held.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Events and wakes captured through the owner hooks.
    ///
    /// With a probe store attached (see [`Captured::hooks_probing`]), each hook also checks that the
    /// store mutex is NOT held when it runs: a hook delivers events and marks a pane dirty, and none
    /// of that belongs inside the store's short critical section.
    #[derive(Clone, Default)]
    struct Captured(
        Arc<StdMutex<Vec<TermEvent>>>,
        Arc<std::sync::atomic::AtomicUsize>,
        Arc<std::sync::atomic::AtomicUsize>,
        Arc<StdMutex<Option<Arc<ImageStore>>>>,
    );

    impl Captured {
        fn hooks(&self, term: &Arc<Mutex<Terminal>>) -> OwnerHooks {
            self.hooks_with(term, None)
        }

        /// Hooks that additionally record whether the store mutex was free at each `wake`.
        fn hooks_probing(
            &self,
            term: &Arc<Mutex<Terminal>>,
            store: &Arc<ImageStore>,
        ) -> OwnerHooks {
            *self.3.lock().unwrap() = Some(store.clone());
            self.hooks_with(term, None)
        }

        fn hooks_with(
            &self,
            term: &Arc<Mutex<Terminal>>,
            probe: Option<Arc<ImageStore>>,
        ) -> OwnerHooks {
            let sink = self.0.clone();
            let wakes = self.1.clone();
            let probe = probe.or_else(|| self.3.lock().unwrap().clone());
            // What a session's hook does: take the events out of the terminal and hand them on. The
            // store hands nothing over any more, so this is where they leave the queue.
            let drain = Arc::downgrade(term);
            let deliver_probe = probe.clone();
            let deliver_held = self.2.clone();
            let wake_held = self.2.clone();
            OwnerHooks {
                term: Arc::downgrade(term),
                deliver: Arc::new(move || {
                    record_held(&deliver_held, deliver_probe.as_deref());
                    let Some(term) = drain.upgrade() else { return };
                    let events = {
                        let mut guard = term.lock();
                        guard.take_events()
                    };
                    sink.lock().unwrap().extend(events);
                }),
                wake: Arc::new(move || {
                    wakes.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    record_held(&wake_held, probe.as_deref());
                }),
            }
        }

        /// How many hooks ran while the probed store's mutex was still held (0 is the rule).
        fn locked_hooks(&self) -> usize {
            self.2.load(std::sync::atomic::Ordering::Relaxed)
        }

        fn events(&self) -> Vec<TermEvent> {
            self.0.lock().unwrap().clone()
        }

        /// Forget what was captured so far: assertions then speak about one transmission only.
        fn clear(&self) {
            self.0.lock().unwrap().clear();
        }

        /// The kitty answers the sink saw, as `(image id, status)` pairs in order — proof that a
        /// client was answered exactly once for each transmission it made.
        fn answers(&self) -> Vec<(u32, String)> {
            let mut out = Vec::new();
            for event in self.events() {
                let TermEvent::PtyWrite(bytes) = event else {
                    continue;
                };
                // Answer frames look like `\x1b_Gi=<id>;<status>\x1b\\`; several can share one write.
                let text = String::from_utf8_lossy(&bytes);
                for part in text.split("\x1b_G") {
                    let Some(rest) = part.strip_prefix("i=") else {
                        continue;
                    };
                    let Some((digits, status)) = rest.split_once(';') else {
                        continue;
                    };
                    let Ok(id) = digits.parse::<u32>() else {
                        continue;
                    };
                    let status: String = status
                        .chars()
                        .take_while(|c| c.is_ascii_alphabetic())
                        .collect();
                    if !status.is_empty() {
                        out.push((id, status));
                    }
                }
            }
            out
        }

        fn wakes(&self) -> usize {
            self.1.load(std::sync::atomic::Ordering::Relaxed)
        }

        fn saw_reply(&self, needle: &[u8]) -> bool {
            self.events().iter().any(|e| {
                matches!(e, TermEvent::PtyWrite(b) if b.windows(needle.len()).any(|w| w == needle))
            })
        }

        fn saw_release(&self, key: ImageKey) -> bool {
            self.events()
                .iter()
                .any(|e| matches!(e, TermEvent::ImageReleased(k) if *k == key))
        }
    }

    #[test]
    fn release_frees_bytes() {
        let store = ImageStore::with_caps(64 * KB, 128 * KB);
        let owner = 7;
        store.submit(owner, ImageKey(1), rgba(4, 4, 9)); // 64 bytes
        assert_books(&store);
        assert_eq!(store.owner_bytes(owner), 64);
        assert_eq!(store.bytes_held(), 64);

        store.release(owner, ImageKey(1));
        assert_books(&store);
        assert_eq!(store.owner_bytes(owner), 0);
        assert_eq!(store.bytes_held(), 0);
        // Releasing a key the terminal already evicted (or never had) must stay harmless.
        store.release(owner, ImageKey(1));
        store.release(999, ImageKey(2));
        assert_eq!(store.bytes_held(), 0);
        assert!(matches!(
            store.get(owner, ImageKey(1), (8, 16), None, false),
            Lookup::Unknown
        ));
    }

    #[test]
    fn caps_hold_across_panes() {
        let per_owner = 48 * KB;
        let total = 72 * KB;
        let store = ImageStore::with_caps(per_owner, total);
        // Each picture is 16 KiB. Two owners push the store past both caps at once.
        let plans = [
            (1u64, [ImageKey(1), ImageKey(2), ImageKey(3)]),
            (2, [ImageKey(4), ImageKey(5), ImageKey(6)]),
        ];
        for (owner, keys) in plans {
            for key in keys {
                store.submit(owner, key, rgba(64, 64, 7));
                store.enforce_caps(owner);
                assert_books(&store);
                assert!(
                    store.owner_bytes(owner) <= per_owner,
                    "owner {owner} over its cap"
                );
                assert!(store.bytes_held() <= total, "app-wide total over its cap");
            }
        }
        // Evicted images stay known (draws answer the placeholder): every key is Ready or
        // Evicted, never vanished.
        for (owner, keys) in plans {
            for key in keys {
                match store.get(owner, key, (8, 16), None, false) {
                    Lookup::Pending | Lookup::Ready(_) | Lookup::Evicted { .. } => {}
                    Lookup::Unknown => panic!("key {key:?} vanished instead of being evicted"),
                }
            }
        }
    }

    #[test]
    fn flood_is_evicted_lru() {
        // 16 KiB pictures, a 32 KiB cap: two survive at a time, and touching one keeps it alive.
        let store = ImageStore::with_caps(32 * KB, 64 * KB);
        let owner = 5;
        // Key 1 is drawn (touched) on every frame; the others are shown once, when submitted.
        for i in 1..=8u64 {
            store.submit(owner, ImageKey(i), rgba(64, 64, 3));
            store.touch(owner, ImageKey(1));
            store.enforce_caps(owner);
            assert_books(&store);
            if i >= 3 {
                // The invariant each frame restores: the redrawn image plus this frame's newest.
                let held = |k: u64| {
                    !matches!(
                        store.get(owner, ImageKey(k), (8, 16), None, false),
                        Lookup::Evicted { .. }
                    )
                };
                // Newest first: a `get` on a ready image marks it shown, and these checks are no
                // exception — asking about the newest one first keeps the invariant intact.
                assert!(
                    held(i) && held(1),
                    "frame {i} should hold the redrawn and newest"
                );
                assert!(
                    !held(i - 1),
                    "frame {i}: the previous frame's image went first"
                );
            }
        }
        let held = |k: u64| {
            !matches!(
                store.get(owner, ImageKey(k), (16, 32), None, false),
                Lookup::Evicted { .. }
            )
        };
        // At the end: the redrawn key and the newest one; everything else went oldest-shown first.
        assert!(held(8));
        assert!(
            held(1),
            "the touched image was shown again, so it outlived newer untouched ones"
        );
        for k in [2u64, 3, 4, 5, 6, 7] {
            assert!(!held(k), "key {k} should have been evicted");
        }
        assert_eq!(store.owner_bytes(owner), 32 * KB);
    }

    /// The eviction order within one owner: resampled copies go before whole images, and only
    /// then do the buffers themselves (oldest shown first).
    #[test]
    fn resamples_are_dropped_before_whole_images() {
        let store = ImageStore::with_caps(48 * KB, 96 * KB);
        let owner = 6;
        for i in 1..=2u64 {
            store.submit(owner, ImageKey(i), rgba(64, 64, 7)); // 16 KiB each
        }
        assert_eq!(store.owner_bytes(owner), 32 * KB);

        // Cache a 32x32 copy of key 1 (4 KiB): the owner sits at 36 KiB, still under its cap.
        assert!(matches!(
            store.get(owner, ImageKey(1), (32, 32), None, false),
            Lookup::Pending
        ));
        store.run_jobs_until_idle();
        assert!(matches!(
            store.get(owner, ImageKey(1), (32, 32), None, false),
            Lookup::Ready(_)
        ));
        assert_books(&store);
        assert_eq!(store.owner_bytes(owner), 36 * KB);

        // One more picture pushes the cap over by 4 KiB: exactly the copy goes, no buffer.
        store.submit(owner, ImageKey(3), rgba(64, 64, 7));
        store.enforce_caps(owner);
        assert_books(&store);
        assert_eq!(store.owner_bytes(owner), 48 * KB);
        for k in [1u64, 2, 3] {
            assert!(
                !matches!(
                    store.get(owner, ImageKey(k), (8, 16), None, false),
                    Lookup::Evicted { .. }
                ),
                "key {k} should still be held: the resample was enough"
            );
        }

        // With no copies left, the next push drops a whole image — the least recently shown one,
        // which is key 1: the checks above drew keys 1, 2 and 3 in that order.
        store.submit(owner, ImageKey(4), rgba(64, 64, 7));
        store.enforce_caps(owner);
        assert_books(&store);
        assert_eq!(store.owner_bytes(owner), 48 * KB);
        assert!(matches!(
            store.get(owner, ImageKey(1), (8, 16), None, false),
            Lookup::Evicted { .. }
        ));
        for k in [2u64, 3, 4] {
            assert!(
                !matches!(
                    store.get(owner, ImageKey(k), (8, 16), None, false),
                    Lookup::Evicted { .. }
                ),
                "key {k} should still be held"
            );
        }
    }

    #[test]
    fn drop_owner_frees_everything_including_queued_work() {
        let store = ImageStore::with_caps(512 * KB, 512 * KB);
        let owner = 3;
        // Encoded payloads queue decode jobs that have not run: the payloads themselves count.
        for i in 1..=3u64 {
            store.submit(owner, ImageKey(i), png(TINY_PNG));
        }
        let queued = store.owner_bytes(owner);
        assert!(queued > 0);
        assert_eq!(store.bytes_held(), queued);

        store.drop_owner(owner);
        assert_books(&store);
        assert_eq!(store.bytes_held(), 0);
        assert_eq!(store.owner_bytes(owner), 0);
        // The queued jobs went with it: running the worker finds nothing to resurrect.
        store.run_jobs_until_idle();
        assert_eq!(store.bytes_held(), 0);
        assert!(matches!(
            store.get(owner, ImageKey(1), (8, 16), None, false),
            Lookup::Unknown
        ));

        // A second drop is harmless.
        store.drop_owner(owner);
    }

    /// The full decode loop against a real terminal: a corrupt payload answers the kitty client
    /// `EBADPNG`, which the owner's `deliver` hook takes out of the terminal's queue, and the worker
    /// keeps serving the next job. A decoder that panics (injected hook) is contained the same way.
    #[test]
    fn corrupt_and_panicking_decodes_are_contained() {
        let term = Arc::new(Mutex::new(Terminal::new(size(), 100)));
        let owner = term.lock().image_owner();
        let store = ImageStore::with_caps(4 * 1024 * 1024, 8 * 1024 * 1024);
        let captured = Captured::default();
        store.register_owner(owner, captured.hooks(&term));

        // A valid PNG header with a cut-short data section: feathercritty lays the placement out
        // and waits for our decode; only the host can call it corrupt.
        let mut broken = TINY_PNG.to_vec();
        broken.truncate(24);
        let feed = format!("\x1b_Gi=1,a=t,f=100;{}\x1b\\", b64(&broken));
        term.lock().feed(feed.as_bytes());
        let key = match term.lock().take_events().first() {
            Some(TermEvent::ImageReceived { key, data }) => {
                assert!(matches!(data, ImageData::Encoded { .. }));
                *key
            }
            other => panic!("expected one encoded image event, got {other:?}"),
        };
        store.submit(owner, key, png(&broken));
        store.run_jobs_until_idle();

        // The client's answer went through the hooks...
        assert!(
            captured.saw_reply(b"EBADPNG"),
            "no EBADPNG reply in {:?}",
            captured.events()
        );
        // ...the failed image is evicted for draws until its release arrives...
        assert!(matches!(
            store.get(owner, key, (8, 16), None, false),
            Lookup::Evicted { .. }
        ));
        // ...and the emulator removed it, so the hooks carried its release too (they drain the
        // terminal's queue when answering, which is where a session sees it).
        assert!(
            captured.saw_release(key),
            "no release for the failed image in {:?}",
            captured.events()
        );
        store.release(owner, key);

        // A panicking decoder (test hook) behaves exactly like a corrupt one... The sink is
        // cleared first so what is checked is this transmission's answer, not the older `EBADPNG`.
        store.set_decode_hook(Some(Arc::new(|data| {
            if matches!(data, ImageData::Encoded { .. }) {
                panic!("test: the decoder panicked")
            }
            None
        })));
        captured.clear();
        let feed = format!("\x1b_Gi=2,a=t,f=100;{}\x1b\\", b64(TINY_PNG));
        term.lock().feed(feed.as_bytes());
        let key2 = match term.lock().take_events().first() {
            Some(TermEvent::ImageReceived { key, .. }) => *key,
            other => panic!("expected an image event, got {other:?}"),
        };
        store.submit(owner, key2, png(TINY_PNG));
        store.run_jobs_until_idle();
        assert_eq!(
            captured.answers(),
            vec![(2, "EBADPNG".to_string())],
            "a panicking decode must answer like a corrupt one, exactly once",
        );
        store.set_decode_hook(None);

        // ...and the worker still serves the next job: a whole PNG now decodes and answers OK.
        captured.clear();
        let feed = format!("\x1b_Gi=3,a=t,f=100;{}\x1b\\", b64(TINY_PNG));
        term.lock().feed(feed.as_bytes());
        let key3 = match term.lock().take_events().first() {
            Some(TermEvent::ImageReceived { key, .. }) => *key,
            other => panic!("expected an image event, got {other:?}"),
        };
        store.submit(owner, key3, png(TINY_PNG));
        store.run_jobs_until_idle();
        assert_eq!(
            captured.answers(),
            vec![(3, "OK".to_string())],
            "no single OK reply after a good decode",
        );
        // The decoded pixels landed: a get fits them into the box and the worker resamples.
        assert!(matches!(
            store.get(owner, key3, (8, 16), None, false),
            Lookup::Pending
        ));
        store.run_jobs_until_idle();
        match store.get(owner, key3, (8, 16), None, false) {
            Lookup::Ready(img) => {
                assert_eq!((img.width, img.height), (8, 8)); // upscaled keeping aspect inside 8x16
                assert_eq!(img.bytes(), 8 * 8 * 4);
            }
            other => panic!("expected a ready resample, got {other:?}"),
        }
    }

    /// The real worker thread: submits reach it through the queue and results come back through
    /// the hooks with no test driving jobs. (A leaked private store stands in for the global one,
    /// which tests must not race.)
    #[test]
    fn the_worker_thread_serves_submits_and_resamples() {
        let term = Arc::new(Mutex::new(Terminal::new(size(), 100)));
        let owner = term.lock().image_owner();
        let store: &'static ImageStore = Box::leak(Box::new(ImageStore::with_caps(
            4 * 1024 * 1024,
            8 * 1024 * 1024,
        )));
        let captured = Captured::default();
        store.register_owner(owner, captured.hooks(&term));

        store.submit(owner, ImageKey(50), png(TINY_PNG));
        store.start();
        assert!(store.wait_idle_for(Duration::from_secs(30)));

        // Submitted directly (no kitty client waiting on this key): the terminal got its answer
        // anyway, and what the owner holds now is the decoded buffer, not the payload.
        assert_eq!(store.owner_bytes(owner), tiny_rgba().len());
        // Cells of 8x16 logical pixels at scale 2: a drawer's 1-cell box is 16x32 physical pixels.
        let box_px = (16, 32);
        match store.get(owner, ImageKey(50), box_px, None, false) {
            Lookup::Pending => {
                assert!(store.wait_idle_for(Duration::from_secs(30)));
                match store.get(owner, ImageKey(50), box_px, None, false) {
                    // A 16x32 physical box, a 4x4 source: upscale keeping aspect → 16x16.
                    Lookup::Ready(img) => assert_eq!((img.width, img.height), (16, 16)),
                    other => panic!("expected a ready resample after the worker, got {other:?}"),
                }
            }
            other => panic!("expected Pending for a first get, got {other:?}"),
        }
    }

    #[test]
    fn registered_hooks_are_replaced_for_a_reconnect() {
        let term = Arc::new(Mutex::new(Terminal::new(size(), 100)));
        let owner = term.lock().image_owner();
        let store = ImageStore::with_caps(64 * KB, 128 * KB);
        let first = Captured::default();
        let second = Captured::default();
        store.register_owner(owner, first.hooks(&term));
        store.register_owner(owner, second.hooks(&term)); // the reconnect's session takes over

        store.submit(owner, ImageKey(60), png(TINY_PNG));
        store.run_jobs_until_idle();
        // A key no kitty client transmitted gets no reply bytes — what proves which session is
        // attached is the wake that asks its pane to redraw with the new pixels.
        assert_eq!(first.wakes(), 0, "the old session was still attached");
        assert_eq!(
            second.wakes(),
            1,
            "the new session should have been reached"
        );
    }

    /// The identity resample (box exactly the source size) returns the fixture's pixels exactly,
    /// read back out of the handle the drawer will draw — and it is the source's own buffer, not a
    /// second frame: the copy accounts nothing (finding P4).
    #[test]
    fn decoded_buffers_survive_the_identity_resample() {
        let store = ImageStore::with_caps(64 * KB, 128 * KB);
        store.submit(1, ImageKey(70), png(TINY_PNG));
        store.run_jobs_until_idle();
        assert!(matches!(
            store.get(1, ImageKey(70), (4, 4), None, false),
            Lookup::Pending
        ));
        store.run_jobs_until_idle();
        match store.get(1, ImageKey(70), (4, 4), None, false) {
            Lookup::Ready(img) => {
                assert_eq!(img.pixels(), tiny_rgba());
                assert_eq!(
                    img.bytes(),
                    tiny_rgba().len(),
                    "one RGBA8 frame of its size"
                );
                assert!(img.shares_source(), "the copy is the source's own pixels");
                assert_eq!(
                    img.charge(),
                    0,
                    "a picture drawn at its own size costs no second frame"
                );
                assert_eq!(
                    store.owner_bytes(1),
                    tiny_rgba().len(),
                    "the decoded frame alone is accounted: the alias adds nothing"
                );
            }
            other => panic!("expected the identity resample, got {other:?}"),
        }
    }

    /// The size a drawer must ask for to draw a picture 1:1 (finding I1): what a decoded buffer
    /// holds, what a payload's header declared while its decode is still queued, and nothing for an
    /// owner or key nobody holds.
    #[test]
    fn source_size_names_the_pixels_a_one_to_one_ask_needs() {
        let store = ImageStore::with_caps(64 * KB, 128 * KB);
        let owner = 5;
        // A payload that has not decoded yet: the header's numbers, which is also what an evicted
        // placeholder names.
        store.submit(owner, ImageKey(1), png(TINY_PNG));
        assert_eq!(store.source_size(owner, ImageKey(1)), Some((4, 4)));
        store.run_jobs_until_idle();
        assert_eq!(
            store.source_size(owner, ImageKey(1)),
            Some((4, 4)),
            "the decoded frame is the same size"
        );

        // Pixels the terminal decoded itself (a sixel's shape): stored as they arrived.
        store.submit(
            owner,
            ImageKey(2),
            ImageData::Rgba {
                width: 4,
                height: 12,
                pixels: vec![0u8; 4 * 12 * 4],
            },
        );
        assert_eq!(store.source_size(owner, ImageKey(2)), Some((4, 12)));

        store.release(owner, ImageKey(2));
        assert_eq!(store.source_size(owner, ImageKey(2)), None, "released");
        assert_eq!(store.source_size(9_999, ImageKey(1)), None, "no such owner");
    }

    /// What `get` makes for a placement that shows only part of an image (`src`): the fit factor
    /// comes from the shown area, so the crop fills its box, but the copy is always the whole
    /// image at that factor — keyed by its size alone. Crops of one shape therefore share one
    /// copy however far apart they sit in the source, and a crop of another shape asks for a size
    /// of its own. `ResampledImage::source` is what lets the drawer tell them apart.
    #[test]
    fn crops_of_one_shape_share_a_copy_and_other_shapes_do_not() {
        let store = ImageStore::with_caps(64 * KB, 512 * KB);
        let owner = 22;
        store.submit(owner, ImageKey(1), rgba(64, 32, 7)); // 8 KiB, whole-image fits below cap
        let rect = |x, y, w, h| Some(Rect { x, y, w, h });
        let box_px = (32, 16);

        // Two placements cropping a 16x8 corner of the picture at the same box: factor 2, so both
        // want the whole image at 128x64. The second ask finds the first's copy and queues nothing.
        for (i, src) in [rect(0, 0, 16, 8), rect(20, 10, 16, 8)]
            .into_iter()
            .enumerate()
        {
            let first = store.get(owner, ImageKey(1), box_px, src, false);
            if i == 0 {
                assert!(matches!(first, Lookup::Pending), "the shape is new");
                store.run_jobs_until_idle();
            } else {
                assert!(
                    matches!(first, Lookup::Ready(_)),
                    "a crop of the same shape reuses the copy: {first:?}"
                );
                assert!(
                    store.inner.lock().jobs.is_empty(),
                    "the shared copy queues no work"
                );
            }
            if let Lookup::Ready(img) = first {
                assert_eq!(
                    (img.width, img.height),
                    (128, 64),
                    "whole image at factor 2"
                );
                assert_eq!(img.source, (64, 32), "the drawer maps src against this");
            }
        }

        // A 32x8 crop of the same picture at the same box: factor 1, so another size and a job.
        assert!(
            matches!(
                store.get(owner, ImageKey(1), box_px, rect(0, 0, 32, 8), false),
                Lookup::Pending
            ),
            "a crop of another shape asks for its own size"
        );
        store.run_jobs_until_idle();
        match store.get(owner, ImageKey(1), box_px, rect(0, 0, 32, 8), false) {
            Lookup::Ready(img) => assert_eq!(img.source, (64, 32)),
            other => panic!("expected the second size, got {other:?}"),
        }
        // Both sizes stay cached beside the source: 8 KiB + 32 KiB. The second copy is the picture
        // at its own size, so it shares the source's buffer and accounts nothing (finding P4).
        assert_eq!(store.owner_bytes(owner), 8 * KB + 32 * KB);
        assert_books(&store);
    }

    /// A stretched ask and a kept-aspect ask of one span are different pictures, so they are two
    /// slots: the fill covers the box (96x64 here), the fit keeps the source's 2:1 ratio (96x48).
    /// Sharing one slot would answer whichever geometry was asked for first and leave the other
    /// placement drawing the wrong size, so both copies are made and both stay cached.
    #[test]
    fn a_stretched_ask_and_a_kept_aspect_ask_of_one_span_do_not_share_a_copy() {
        let store = ImageStore::with_caps(64 * KB, 512 * KB);
        let owner = 23;
        store.submit(owner, ImageKey(1), rgba(32, 16, 7)); // 2 KiB at 2:1
        let box_px = (96, 64);

        assert!(
            matches!(
                store.get(owner, ImageKey(1), box_px, None, false),
                Lookup::Pending
            ),
            "the fit is asked for first"
        );
        store.run_jobs_until_idle();
        match store.get(owner, ImageKey(1), box_px, None, false) {
            Lookup::Ready(img) => assert_eq!(
                (img.width, img.height),
                (96, 48),
                "a kept-aspect copy fits the box and keeps the source's ratio"
            ),
            other => panic!("expected the fitted copy, got {other:?}"),
        }

        // The same box asked as a fill. Answering it from the slot above would draw 96x48 in a span
        // 16 px taller than the picture, which is exactly what `preserveAspectRatio=0` rules out.
        assert!(
            matches!(
                store.get(owner, ImageKey(1), box_px, None, true),
                Lookup::Pending
            ),
            "a stretched ask does not reuse the kept-aspect copy"
        );
        store.run_jobs_until_idle();
        match store.get(owner, ImageKey(1), box_px, None, true) {
            Lookup::Ready(img) => assert_eq!(
                (img.width, img.height),
                (96, 64),
                "a stretched copy is the whole box"
            ),
            other => panic!("expected the stretched copy, got {other:?}"),
        }

        // Each ask now answers from its own slot and queues nothing.
        for stretch in [false, true] {
            assert!(
                matches!(
                    store.get(owner, ImageKey(1), box_px, None, stretch),
                    Lookup::Ready(_)
                ),
                "the copy asked for with stretch={stretch} is cached"
            );
            assert!(
                store.inner.lock().jobs.is_empty(),
                "a cached size queues no work"
            );
        }
        // The source plus both copies: 2 KiB + 18 KiB + 24 KiB.
        assert_eq!(store.owner_bytes(owner), 2 * KB + 18 * KB + 24 * KB);
        assert_books(&store);
    }

    /// An uncropped placement fits the whole image inside its box, exactly as before: a 64x32
    /// picture in a 16x8 box becomes 16x8, and one huge box cannot ask for more than a frame.
    #[test]
    fn decoded_fits_are_capped_at_one_frame() {
        let big = Rgba8Image {
            width: 10_000,
            height: 10_000,
            pixels: bytes::Bytes::new(),
        };
        // A huge box on a large source: the copy never exceeds one 64 MiB RGBA8 frame.
        let fitted = fit_target(&big, None, (u32::MAX, u32::MAX));
        assert!((fitted.0 as u64) * u64::from(fitted.1) * 4 <= 64 * 1024 * 1024);
        assert!(fitted.0 >= 1 && fitted.1 >= 1);
        // A normal box is unaffected: exact scales pass through.
        let small = Rgba8Image {
            width: 8,
            height: 8,
            pixels: bytes::Bytes::new(),
        };
        assert_eq!(fit_target(&small, None, (16, 16)), (16, 16));
        // The shown area, not the whole picture, decides the factor.
        let wide = Rgba8Image {
            width: 64,
            height: 32,
            pixels: bytes::Bytes::new(),
        };
        assert_eq!(fit_target(&wide, None, (16, 8)), (16, 8));
        let strip = Some(Rect {
            x: 0,
            y: 0,
            w: 4,
            h: 32,
        });
        assert_eq!(
            fit_target(&wide, strip, (16, 8)),
            (16, 8),
            "a 4x32 crop in a 16x8 box: factor 0.25 applied to the whole picture"
        );
        // A zero side means "to the end", and a rectangle past the edge still yields one pixel.
        assert_eq!(src_area(strip, 64, 32), (4, 32));
        assert_eq!(src_area(None, 64, 32), (64, 32));
        assert_eq!(
            src_area(
                Some(Rect {
                    x: 100,
                    y: 100,
                    w: 8,
                    h: 8,
                }),
                64,
                32,
            ),
            (1, 1),
            "outside the picture: never zero"
        );
    }

    /// An evicted picture's placeholder names its source size (spec 2.6): a decoded buffer dropped
    /// by the caps, a size refused beside a picture that is still held, and a payload evicted
    /// before its decode ran all answer the same numbers the program sent.
    #[test]
    fn evicted_lookups_name_the_source_size() {
        let per_owner = 16 * KB + 512; // a 16 KiB picture fits alone, never beside a copy
        let store = ImageStore::with_caps(per_owner, 8 * per_owner);
        let owner = 23;
        store.submit(owner, ImageKey(1), rgba(64, 64, 1));
        assert!(matches!(
            store.get(owner, ImageKey(1), (16, 16), None, false),
            Lookup::Pending
        ));
        store.run_jobs_until_idle();
        match store.get(owner, ImageKey(1), (16, 16), None, false) {
            Lookup::Evicted { width, height } => assert_eq!((width, height), (64, 64)),
            other => panic!("a refused size answers the placeholder, got {other:?}"),
        }

        // A payload evicted while it waits to decode: the header's dimensions, which the terminal
        // reported with the bytes.
        let tiny = ImageStore::with_caps(100, 8 * KB);
        let owner2 = 24;
        for i in [ImageKey(1), ImageKey(2)] {
            tiny.submit(owner2, i, png(TINY_PNG));
        }
        tiny.enforce_caps(owner2);
        match tiny.get(owner2, ImageKey(1), (8, 16), None, false) {
            Lookup::Evicted { width, height } => assert_eq!((width, height), (4, 4)),
            other => panic!("the evicted payload should stay known, got {other:?}"),
        }
    }

    /// The caps must never strand a client: a key evicted while its decode is still queued is
    /// answered exactly once anyway — `ENOMEM` — and the emulator then releases it, so its
    /// pending-decode slot comes back. Four tiny PNG transmissions against a cap that holds two
    /// payloads; the worker's queue ends up holding two decodes and two refusals.
    #[test]
    fn evicted_pending_decodes_answer_once_and_release() {
        let term = Arc::new(Mutex::new(Terminal::new(size(), 100))); // 80x24: nothing scrolls off
        let owner = term.lock().image_owner();
        let store = ImageStore::with_caps(300, 4 * KB); // a tiny PNG payload is 136 bytes
        let captured = Captured::default();
        let weak = Arc::downgrade(&term);
        store.register_owner(owner, captured.hooks(&term));

        let mut keys = Vec::new();
        for id in 1..=4u32 {
            let feed = format!("\x1b_Gi={id},a=t,f=100;{}\x1b\\", b64(TINY_PNG));
            term.lock().feed(feed.as_bytes());
            term.lock().tick(Instant::now());
            // Bound the events so the terminal lock is released before reclaim re-locks it.
            let events = term.lock().take_events();
            for event in events {
                if let TermEvent::ImageReceived { key, data } = event {
                    assert_eq!(keys.len(), usize::try_from(id).unwrap() - 1);
                    keys.push((id, key));
                    store.submit(owner, key, data);
                    // Exactly what a session does after a submit: the shared helper, then the wakes
                    // it left for the caller.
                    store.wake_owners(&reclaim(&store, &weak, owner));
                }
            }
        }
        assert_eq!(
            keys.len(),
            4,
            "every transmission should have reached the store"
        );
        assert_eq!(
            store.owner_bytes(owner),
            2 * TINY_PNG.len(),
            "two payloads fit under the cap; the older two were evicted while pending",
        );

        // Nothing has decoded yet: run the queue, which now holds two decodes and two refusals.
        store.run_jobs_until_idle();
        assert_books(&store);
        let mut answers = captured.answers();
        answers.sort();
        assert_eq!(
            answers,
            vec![
                (1, "ENOMEM".to_string()),
                (2, "ENOMEM".to_string()),
                (3, "OK".to_string()),
                (4, "OK".to_string()),
            ],
            "one answer per transmission, refusals for the evicted payloads",
        );
        // The refused keys are drawn as placeholders until released, and the emulator — told the
        // decode failed — removed their placements and released them.
        for (id, key) in &keys[..2] {
            assert!(
                matches!(
                    store.get(owner, *key, (8, 16), None, false),
                    Lookup::Evicted { .. }
                ),
                "key {id} should stay known as evicted",
            );
            assert!(
                captured.saw_release(*key),
                "no release for refused key {id}"
            );
        }
        for (id, key) in &keys[2..] {
            assert!(
                !matches!(
                    store.get(owner, *key, (8, 16), None, false),
                    Lookup::Evicted { .. }
                ),
                "key {id} decoded",
            );
        }
        // The two decoded buffers are what remains accounted; the refused payloads went with their
        // eviction.
        assert_books(&store);
        assert_eq!(store.owner_bytes(owner), 2 * tiny_rgba().len());
        assert_eq!(store.bytes_held(), 2 * tiny_rgba().len());
    }

    /// Repeated draws at new placement sizes never push the store over its cap: every install is
    /// enforced, and asking for a new size keeps the copies made for other sizes — revisiting one
    /// costs no work as long as it still fits.
    #[test]
    fn resample_copies_never_push_the_store_over_cap() {
        let per_owner = 20 * KB; // a 16 KiB picture plus room for the copies asked for below
        let total = 40 * KB;
        let store = ImageStore::with_caps(per_owner, total);
        let owner = 8;
        store.submit(owner, ImageKey(1), rgba(64, 64, 7));
        assert_eq!(store.owner_bytes(owner), 16 * KB);

        for cells in [(16u32, 16u32), (12, 12), (8, 8), (20, 20), (4, 4)] {
            if matches!(
                store.get(owner, ImageKey(1), (cells.0, cells.1), None, false),
                Lookup::Pending
            ) {
                store.run_jobs_until_idle();
            }
            assert_books(&store);
            assert!(
                store.owner_bytes(owner) <= per_owner,
                "the per-owner cap blew after asking for a {}x{} box",
                cells.0,
                cells.1,
            );
            assert!(store.bytes_held() <= total, "the app-wide total blew");
        }

        // All five sizes (3,520 bytes of copies in all) fit beside the source, so all stay cached:
        // nothing about asking for one size drops another. Re-asking the first one costs no job.
        assert!(
            matches!(
                store.get(owner, ImageKey(1), (4, 4), None, false),
                Lookup::Ready(_)
            ),
            "the last-asked size should be the cached one",
        );
        let asked_first = store.get(owner, ImageKey(1), (16, 16), None, false);
        assert!(
            matches!(asked_first, Lookup::Ready(_)),
            "the first-asked size is still cached: `get` drops nothing",
        );
        assert!(store.inner.lock().jobs.is_empty());
        assert_eq!(store.owner_bytes(owner), 16 * KB + 3_520);
    }

    /// One picture drawn alternately at two sizes, round after round: each size is resampled
    /// exactly once and then stays cached forever. Nothing about alternating — no supersede, no
    /// install-time eviction of the other size — makes the worker re-make a copy it just made.
    #[test]
    fn alternating_sizes_resample_exactly_once_each() {
        // A leaked private store stands in for the global one (tests must not race for it).
        let store: &'static ImageStore =
            Box::leak(Box::new(ImageStore::with_caps(64 * KB, 256 * KB)));
        let owner = 12;
        store.submit(owner, ImageKey(1), rgba(64, 64, 7)); // 16 KiB source
        store.start();

        let mut pending_asks = 0;
        for round in 0..12 {
            for cell_px in [(16u32, 16u32), (32, 32)] {
                if matches!(
                    store.get(owner, ImageKey(1), cell_px, None, false),
                    Lookup::Pending
                ) {
                    pending_asks += 1; // this ask queued a resample
                }
                assert!(
                    store.wait_idle_for(Duration::from_secs(30)),
                    "the worker should drain the queue every round"
                );
                assert!(
                    matches!(
                        store.get(owner, ImageKey(1), cell_px, None, false),
                        Lookup::Ready(_)
                    ),
                    "size {cell_px:?} should be cached by round {round}"
                );
            }
        }
        assert_eq!(pending_asks, 2, "one resample per size, ever");
        assert!(store.inner.lock().jobs.is_empty(), "the queue must idle");
        // Whole (16 KiB) plus both copies (1 KiB and 4 KiB).
        assert_eq!(store.owner_bytes(owner), 16 * KB + 1024 + 4096);
        assert_books(store);
    }

    /// Enforcement triggered by installing a resample protects the copy just made and its source:
    /// an older whole picture pays for it, and the next draw finds the copy ready with nothing
    /// queued.
    #[test]
    fn install_enforcement_evicts_an_older_picture_not_the_new_copy() {
        let per_owner = 3 * 16 * KB; // exactly three whole 16 KiB pictures
        let store = ImageStore::with_caps(per_owner, 8 * per_owner);
        let owner = 13;
        for i in 1..=3u64 {
            store.submit(owner, ImageKey(i), rgba(64, 64, 3)); // key 3 is the newest shown
        }
        assert_eq!(store.owner_bytes(owner), per_owner);
        assert_books(&store);

        // A 16x16 copy (1 KiB) of the most recently shown picture pushes the owner over its cap.
        assert!(matches!(
            store.get(owner, ImageKey(3), (16, 16), None, false),
            Lookup::Pending
        ));
        store.run_jobs_until_idle();
        assert_books(&store);

        // The copy is there — key 1, the least recently shown picture, paid for it. Both the new
        // copy and its source were protected from this enforcement, so neither could be the victim:
        // exactly 1 KiB went over, and only a whole picture frees that much.
        assert!(matches!(
            store.get(owner, ImageKey(3), (16, 16), None, false),
            Lookup::Ready(_)
        ));
        assert!(matches!(
            store.get(owner, ImageKey(1), (8, 16), None, false),
            Lookup::Evicted { .. }
        ));
        // One more ask: still Ready, and nothing queued behind it — no install, no eviction churn.
        assert!(matches!(
            store.get(owner, ImageKey(3), (16, 16), None, false),
            Lookup::Ready(_)
        ));
        assert!(store.inner.lock().jobs.is_empty());
        assert!(store.owner_bytes(owner) <= per_owner);
        assert_books(&store);
    }

    /// A copy that cannot fit beside its own source is refused — up front when geometry alone
    /// proves it (see [`ImageStore::finish_resample`]), or by dropping back out when enforcement
    /// gets only as far as "nothing but the protected picture" — with one `wake` for the asking
    /// pane (refused is an answer: it draws the placeholder), and the refusal remembered for that
    /// `(key, target)`: every later draw answers `Evicted` without queueing, until the source image
    /// is replaced or released. That memory is the chosen loop guard: re-asking a size that provably
    /// cannot fit would burn the same work forever.
    #[test]
    fn a_copy_that_cannot_fit_is_refused_once_and_stays_refused() {
        let per_owner = 16 * KB + 512; // room for a 16 KiB picture, not for a copy beside it
        let store = ImageStore::with_caps(per_owner, 8 * per_owner);
        let owner = 14;
        let term = Arc::new(Mutex::new(Terminal::new(size(), 100)));
        let captured = Captured::default();
        store.register_owner(owner, captured.hooks(&term));

        store.submit(owner, ImageKey(1), rgba(64, 64, 1)); // 16 KiB: fits alone, under the cap
        assert_books(&store);

        // A 16x16 copy (1 KiB): source plus copy exceed the cap on geometry alone, so it is
        // refused outright.
        assert!(matches!(
            store.get(owner, ImageKey(1), (16, 16), None, false),
            Lookup::Pending
        ));
        store.run_jobs_until_idle(); // the only `Pending` this ask will ever produce
        assert_books(&store);

        for _ in 0..4 {
            assert!(
                matches!(
                    store.get(owner, ImageKey(1), (16, 16), None, false),
                    Lookup::Evicted { .. }
                ),
                "the refused size answers the placeholder without queueing again",
            );
        }
        store.run_jobs_until_idle();
        assert!(
            store.inner.lock().jobs.is_empty(),
            "a refused size queues nothing"
        );
        assert_eq!(
            captured.wakes(),
            1,
            "the refusal is an answer the pane has to redraw for: it draws the placeholder"
        );
        assert_eq!(store.owner_bytes(owner), 16 * KB);
        assert!(store.owner_bytes(owner) <= per_owner);

        // The refusal belongs to the source entry: a new picture under the same key is asked
        // afresh (and refused afresh — the geometry has not changed).
        store.submit(owner, ImageKey(1), rgba(64, 64, 2));
        assert!(matches!(
            store.get(owner, ImageKey(1), (16, 16), None, false),
            Lookup::Pending
        ));
        store.run_jobs_until_idle();
        assert!(matches!(
            store.get(owner, ImageKey(1), (16, 16), None, false),
            Lookup::Evicted { .. }
        ));
        assert_books(&store);
    }

    /// A copy that could never fit beside its own source is refused up front: no enforcement pass
    /// runs for it, so nothing else — not even a small neighbour — is evicted to make room for a
    /// buffer that would be dropped anyway.
    #[test]
    fn an_upfront_refusal_evicts_no_neighbours() {
        let per_owner = 16 * KB + 512; // the source alone fits; source plus any copy never does
        let store = ImageStore::with_caps(per_owner, 8 * per_owner);
        let owner = 16;
        let term = Arc::new(Mutex::new(Terminal::new(size(), 100)));
        let captured = Captured::default();
        store.register_owner(owner, captured.hooks(&term));

        store.submit(owner, ImageKey(1), rgba(64, 64, 1)); // 16 KiB source
        store.submit(owner, ImageKey(2), rgba(8, 8, 3)); // a 256-byte neighbour
        assert_eq!(store.owner_bytes(owner), 16 * KB + 256);
        assert_books(&store);

        // 16 KiB + 1 KiB exceeds the owner's cap on its own: refused at once, nothing evicted.
        assert!(matches!(
            store.get(owner, ImageKey(1), (16, 16), None, false),
            Lookup::Pending
        ));
        store.run_jobs_until_idle();
        assert_books(&store);
        {
            let inner = store.inner.lock();
            assert!(
                matches!(
                    inner.owners[&owner].images[&ImageKey(2)].state,
                    State::Ready(_)
                ),
                "the neighbour stayed held: an impossible copy is refused without evicting for it",
            );
        }
        assert!(matches!(
            store.get(owner, ImageKey(1), (16, 16), None, false),
            Lookup::Evicted { .. }
        ));
        assert_eq!(
            captured.wakes(),
            1,
            "a refusal wakes the asking pane once, so its empty box becomes the placeholder"
        );
        assert_eq!(
            store.owner_bytes(owner),
            16 * KB + 256,
            "byte counts untouched"
        );
    }

    /// A refused size is an answer, not a silence. The pane that asked parked its frame on
    /// `pending`, and only asking again draws the `[image W×H]` line spec 2.6 and 6.3 put in an
    /// empty box — so the refusal wakes its owner, with the store mutex released (a hook delivers
    /// events and marks a pane dirty; none of that belongs in the store's short critical section).
    /// The refused slot then answers without queueing, so this cannot become a loop.
    #[test]
    fn a_refusal_wakes_its_owner_with_the_store_unlocked() {
        let per_owner = 16 * KB + 512; // the source alone fits; source plus any copy never does
        let store = Arc::new(ImageStore::with_caps(per_owner, 8 * per_owner));
        let owner = 21;
        let term = Arc::new(Mutex::new(Terminal::new(size(), 100)));
        let captured = Captured::default();
        store.register_owner(owner, captured.hooks_probing(&term, &store));

        store.submit(owner, ImageKey(1), rgba(64, 64, 1)); // 16 KiB
        assert!(matches!(
            store.get(owner, ImageKey(1), (16, 16), None, false),
            Lookup::Pending
        ));
        assert_eq!(store.generation(owner), 0, "asking changes nothing");
        store.run_jobs_until_idle(); // refused: the copy cannot fit beside its source
        assert_eq!(captured.wakes(), 1, "the refusal reaches the pane");
        assert_eq!(
            store.generation(owner),
            1,
            "and it moves the epoch too: only a rebuilt frame draws the placeholder"
        );
        assert_eq!(
            captured.locked_hooks(),
            0,
            "the hook ran with no store lock held"
        );

        // The redraw asks again and is answered at once, without queueing or waking a second time.
        assert!(matches!(
            store.get(owner, ImageKey(1), (16, 16), None, false),
            Lookup::Evicted { .. }
        ));
        store.run_jobs_until_idle();
        assert_eq!(captured.wakes(), 1, "one refusal wakes the pane once");
        assert_eq!(
            store.generation(owner),
            1,
            "the re-ask changes nothing more"
        );
        assert_books(&store);
    }

    /// A picture taken from an owner by cap pressure — even pressure another pane caused — wakes
    /// that owner. Its cached draw list holds a handle for pixels the store just freed: only asking
    /// again replaces the picture with its placeholder and lets iced drop the texture, which is how
    /// the caps stay the real bound on memory rather than a number the visible panes exceed.
    #[test]
    fn an_eviction_wakes_the_owner_it_took_from() {
        // Room for one 4 KiB picture in all: enforcement takes the triggering owner's own first and
        // then the other's, and both must hear about it.
        let store = Arc::new(ImageStore::with_caps(8 * KB, 2 * KB));
        let (trigger, other) = (22, 23);
        let t_term = Arc::new(Mutex::new(Terminal::new(size(), 100)));
        let o_term = Arc::new(Mutex::new(Terminal::new(size(), 100)));
        let mine = Captured::default();
        let theirs = Captured::default();
        store.register_owner(trigger, mine.hooks_probing(&t_term, &store));
        store.register_owner(other, theirs.hooks_probing(&o_term, &store));

        store.submit(other, ImageKey(1), rgba(32, 32, 7)); // shown first
        store.submit(trigger, ImageKey(1), rgba(32, 32, 9));
        assert_eq!(store.owner_bytes(other), 4 * KB);
        store.enforce_caps(trigger); // what a submit or a finished decode does afterwards

        assert_eq!(
            store.owner_bytes(trigger),
            0,
            "the trigger's own picture went first"
        );
        assert_eq!(
            store.owner_bytes(other),
            0,
            "then the older one to free the cap"
        );
        assert_eq!(
            theirs.wakes(),
            1,
            "the pane whose picture was taken is asked to redraw"
        );
        assert_eq!(
            mine.wakes(),
            1,
            "and so is the one enforcement freed room from"
        );
        assert_eq!(
            theirs.locked_hooks() + mine.locked_hooks(),
            0,
            "no hook ran under the store mutex"
        );
        assert_books(&store);
    }

    /// The notices a caps reclaim produces can be collected without being sent (finding M3): a
    /// caller holding a lock of its own — a session router holding its routing table, which the
    /// feeder thread that reads those deliveries may be waiting on — takes the owners list, lets go
    /// of its lock, and only then asks the panes to redraw.
    #[test]
    fn caps_reclaim_notices_can_be_deferred() {
        let store = Arc::new(ImageStore::with_caps(8 * KB, 2 * KB));
        let (trigger, other) = (30, 31);
        let t_term = Arc::new(Mutex::new(Terminal::new(size(), 100)));
        let o_term = Arc::new(Mutex::new(Terminal::new(size(), 100)));
        let mine = Captured::default();
        let theirs = Captured::default();
        store.register_owner(trigger, mine.hooks_probing(&t_term, &store));
        store.register_owner(other, theirs.hooks_probing(&o_term, &store));

        store.submit(other, ImageKey(1), rgba(32, 32, 7)); // shown first
        store.submit(trigger, ImageKey(1), rgba(32, 32, 9));

        let wake = store.collect_caps_reclaim(trigger);
        assert_eq!(
            store.owner_bytes(trigger),
            0,
            "the trigger's own picture went"
        );
        assert_eq!(store.owner_bytes(other), 0, "then the older one");
        assert_eq!(
            mine.wakes() + theirs.wakes(),
            0,
            "collecting the notices sends nothing"
        );

        // A caller would release its own lock here, then wake.
        let mut wake = wake;
        wake.sort_unstable();
        wake.dedup();
        assert_eq!(wake, vec![trigger, other], "both owners are named");
        store.wake_owners(&wake);
        assert_eq!(mine.wakes(), 1, "the triggering pane redraws");
        assert_eq!(theirs.wakes(), 1, "and so does the pane whose picture went");
        assert_eq!(
            theirs.locked_hooks() + mine.locked_hooks(),
            0,
            "no hook ran under the store mutex"
        );
        assert_books(&store);
    }

    /// A frame reused without asking the store reports what it still shows (`ImageStore::touch_drawn`),
    /// or the LRU's idea of "least recently shown" drifts from what is on screen: a pane sitting on
    /// one picture while another fills the caps would lose that picture mid-view.
    #[test]
    fn touch_drawn_keeps_a_visible_picture_through_lru() {
        let per_owner = 8 * KB + 512; // room for two 4 KiB pictures, not three
        let store = ImageStore::with_caps(per_owner, 16 * KB);
        let owner = 24;
        store.submit(owner, ImageKey(1), rgba(32, 32, 1)); // shown first
        store.submit(owner, ImageKey(2), rgba(32, 32, 2));
        // The pane still draws key 1 — its frame was reused, so it never asked for it.
        store.touch_drawn(owner, &[ImageKey(1)]);
        store.submit(owner, ImageKey(3), rgba(32, 32, 3)); // the third does not fit
        store.enforce_caps(owner);
        {
            let inner = store.inner.lock();
            assert!(
                matches!(
                    inner.owners[&owner].images[&ImageKey(1)].state,
                    State::Ready(_)
                ),
                "the visible picture survived: reporting it shown made it the newest"
            );
            assert!(
                matches!(
                    inner.owners[&owner].images[&ImageKey(2)].state,
                    State::Evicted
                ),
                "the picture nobody drew went instead"
            );
        }
        // An unknown key is skipped, not an error; an empty list takes no lock at all.
        store.touch_drawn(owner, &[ImageKey(9)]);
        store.touch_drawn(999, &[ImageKey(1)]);
        assert_books(&store);
    }

    /// The filter rule is asked about the pair the resample actually scales — `whole` to `target` —
    /// so a 2x zoom of a pixel-art sheet stays nearest neighbour even when the placement crops it to
    /// another aspect. The queued job shows the decision; the finished copy proves it, with the
    /// sheet's hard edge still hard (Lanczos3 rings beside an edge like this).
    #[test]
    fn an_integer_zoom_of_a_crop_uses_nearest() {
        let store = ImageStore::with_caps(64 * KB, 128 * KB);
        let owner = 25;
        // A 64x32 sheet: opaque black on the left half, white on the right.
        let mut pixels = vec![0u8; 64 * 32 * 4];
        for px in pixels.as_chunks_mut::<4>().0 {
            px[3] = 255; // fully opaque everywhere: what differs is the colour, not the alpha
        }
        for y in 0..32usize {
            for x in 32..64usize {
                let i = (y * 64 + x) * 4;
                pixels[i] = 255;
                pixels[i + 1] = 255;
                pixels[i + 2] = 255;
            }
        }
        store.submit(
            owner,
            ImageKey(1),
            ImageData::Rgba {
                width: 64,
                height: 32,
                pixels,
            },
        );

        // A 16x16 tile of the black half, in a 32x32 box: factor 2, so the copy is the whole sheet
        // at 128x64 — an exact 2x zoom of ITSELF, whatever the crop's own aspect was.
        let src = Some(Rect {
            x: 0,
            y: 0,
            w: 16,
            h: 16,
        });
        assert!(matches!(
            store.get(owner, ImageKey(1), (32, 32), src, false),
            Lookup::Pending
        ));
        let integer = {
            let inner = store.inner.lock();
            match inner.jobs.front() {
                Some(Job::Resample {
                    target, integer, ..
                }) => {
                    assert_eq!(
                        *target,
                        (128, 64),
                        "the whole sheet at the crop's fit factor"
                    );
                    *integer
                }
                Some(_) => panic!("the queued job is not a resample"),
                None => panic!("nothing was queued for the new shape"),
            }
        };
        assert!(integer, "64x32 to 128x64 is an integer multiple");
        store.run_jobs_until_idle();

        match store.get(owner, ImageKey(1), (32, 32), src, false) {
            Lookup::Ready(img) => {
                assert_eq!((img.width, img.height), (128, 64));
                let px = img.pixels();
                // The sheet's edge lands between copy columns 63 and 64: nearest repeats each source
                // pixel, so both stay exactly what they were.
                let at = |x: usize| [px[x * 4], px[x * 4 + 1], px[x * 4 + 2], px[x * 4 + 3]];
                assert_eq!(at(63), [0, 0, 0, 255], "the last black column");
                assert_eq!(at(64), [255, 255, 255, 255], "the first white one");
            }
            other => panic!("expected the finished copy, got {other:?}"),
        }
        assert_books(&store);
    }

    /// An eviction moves the evicted owners' generation — the number the widget's frame-cache key
    /// carries — and nobody else's. Without it the woken pane reuses its cached frame: same key, so
    /// same `DrawList`, so the picture the store just freed stays drawn from a cloned handle (and
    /// `touch_drawn` re-stamps it), keeping exactly the bytes the caps meant to reclaim alive for as
    /// long as the idle pane shows no output. The bump lands under the store lock, before the wake
    /// goes out, so a rebuild caused by that wake always sees the new number.
    #[test]
    fn an_eviction_bumps_the_generation_of_its_owners_and_nobody_elses() {
        // Three 4 KiB pictures against a 6 KiB total: enforcement takes the trigger's own first,
        // then the oldest shown among the others — and stops there.
        let store = ImageStore::with_caps(16 * KB, 6 * KB);
        let (victim, untouched, trigger) = (30, 31, 32);
        store.submit(victim, ImageKey(1), rgba(32, 32, 7)); // shown first
        store.submit(untouched, ImageKey(1), rgba(32, 32, 8));
        store.submit(trigger, ImageKey(1), rgba(32, 32, 9));
        for owner in [victim, untouched, trigger] {
            assert_eq!(store.generation(owner), 0, "submits change nothing");
        }

        store.enforce_caps(trigger);

        assert_eq!(store.generation(trigger), 1, "its own picture went first");
        assert_eq!(
            store.generation(victim),
            1,
            "the owner shown the second evicted picture must rebuild too"
        );
        assert_eq!(
            store.generation(untouched),
            0,
            "the pane nobody touched keeps its frame — no spurious rebuilds"
        );
        // A never-seen owner reads 0; a dropped one starts over with the next registration.
        assert_eq!(store.generation(999), 0);
        store.drop_owner(victim);
        assert_eq!(
            store.generation(victim),
            0,
            "un-registering drops the counter"
        );
        assert_books(&store);
    }

    /// Everything the draw path does — lookups, `touch_drawn`, and even a satisfied resample install
    /// — leaves the generation alone. Only evictions and refusals move it, so the epoch invalidates
    /// exactly the frames that hold stale handles and never turns every redraw into a rebuild loop.
    #[test]
    fn lookups_touches_and_a_finished_copy_bump_nothing() {
        let store = ImageStore::with_caps(64 * KB, 128 * KB);
        let owner = 33;
        store.submit(owner, ImageKey(1), rgba(64, 64, 1)); // 16 KiB

        // A resample asked for and satisfied: the install wakes its asking pane (that frame was
        // `pending` and rebuilds on its own key), but nothing was taken from anyone.
        assert!(matches!(
            store.get(owner, ImageKey(1), (16, 16), None, false),
            Lookup::Pending
        ));
        store.run_jobs_until_idle();
        assert!(matches!(
            store.get(owner, ImageKey(1), (16, 16), None, false),
            Lookup::Ready(_)
        ));
        // Stamping the on-screen keys for a reused frame, repeats included.
        store.touch_drawn(owner, &[ImageKey(1)]);
        store.touch_drawn(owner, &[ImageKey(1)]);
        assert_eq!(
            store.generation(owner),
            0,
            "the draw path never moves the epoch"
        );
        assert_books(&store);
    }

    /// A decode that finishes with no hooks registered still enforces the caps: the installed
    /// buffer counts against them, and leaving the owner over until the next submit would make
    /// enforcement depend on traffic that may never come. Scrollback reclaim needs a terminal, so
    /// this path is plain `enforce_caps` — and every owed answer stays stashed for registration.
    #[test]
    fn hookless_decode_completions_enforce_the_caps() {
        // A tiny PNG payload is 136 bytes; its decoded buffer is 64. Two payloads never fit
        // under this cap, and submit deliberately leaves the owner over.
        let store = ImageStore::with_caps(100, 8 * KB);
        let owner = 17;
        store.submit(owner, ImageKey(1), png(TINY_PNG));
        store.submit(owner, ImageKey(2), png(TINY_PNG));
        assert_eq!(store.owner_bytes(owner), 2 * TINY_PNG.len());
        assert!(store.over_cap(owner));
        assert_books(&store);

        // No hooks, no terminal: each decode finishes into a stashed answer, and enforcement runs
        // anyway — LRU takes the oldest first, and every evicted owed key queues its refusal.
        store.run_jobs_until_idle();
        assert!(
            !store.over_cap(owner),
            "the caps hold as soon as the decodes resolve"
        );
        assert_eq!(store.bytes_held(), 0);
        assert_books(&store);
        {
            let inner = store.inner.lock();
            let entry = &inner.owners[&owner];
            for key in [ImageKey(1), ImageKey(2)] {
                let image = &entry.images[&key];
                assert!(
                    matches!(image.state, State::Evicted),
                    "key {key:?} was evicted"
                );
                assert!(
                    image.owes_reply && image.pending_answer.is_some(),
                    "the answer stays owed and waiting for a registration",
                );
            }
        }
    }

    /// Copies of on-screen images under cap pressure settle instead of evicting each other every
    /// frame: the install that displaces another image's copy marks that size refused, so its
    /// drawer draws the placeholder and never re-queues it (spec 6.3's placeholder rule).
    #[test]
    fn install_time_evictions_of_other_copies_are_refused_not_remade() {
        // Two whole 16 KiB pictures plus exactly one 1 KiB copy fit; both copies together do not.
        let per_owner = 2 * 16 * KB + 1024;
        let store: &'static ImageStore =
            Box::leak(Box::new(ImageStore::with_caps(per_owner, 8 * per_owner)));
        let owner = 15;
        store.submit(owner, ImageKey(1), rgba(64, 64, 1));
        store.submit(owner, ImageKey(2), rgba(64, 64, 2));
        store.start();

        let mut pending_asks = 0;
        for round in 0..12 {
            for key in [ImageKey(1), ImageKey(2)] {
                if matches!(
                    store.get(owner, key, (16, 16), None, false),
                    Lookup::Pending
                ) {
                    pending_asks += 1; // this ask queued a resample
                }
                assert!(
                    store.wait_idle_for(Duration::from_secs(30)),
                    "the worker should idle every round (round {round})"
                );
            }
        }
        // Exactly two resamples ever: the second install evicted the first copy, and that
        // eviction marked the losing size refused — so no later ask queued a third.
        assert_eq!(pending_asks, 2, "the two copies must settle after round 2");
        assert!(store.inner.lock().jobs.is_empty());

        // The last-installed copy is Ready; the displaced one answers its placeholder. Both whole
        // images are still held (only sizes are refused, not pictures), and the books balance.
        assert!(
            matches!(
                store.get(owner, ImageKey(2), (16, 16), None, false),
                Lookup::Ready(_)
            ),
            "the newest copy stays",
        );
        assert!(
            matches!(
                store.get(owner, ImageKey(1), (16, 16), None, false),
                Lookup::Evicted { .. }
            ),
            "the evicted size is refused, not re-made",
        );
        {
            let inner = store.inner.lock();
            let entry = &inner.owners[&owner];
            for key in [ImageKey(1), ImageKey(2)] {
                assert!(
                    matches!(entry.images[&key].state, State::Ready(_)),
                    "whole images stay held",
                );
            }
        }
        assert_eq!(store.owner_bytes(owner), per_owner); // two whole + one copy
        assert_books(store);
    }

    /// Two copies that cannot coexist settle with the first's size refused under pressure. A
    /// release of an unrelated picture that actually existed frees memory, so that refused size
    /// comes back: the next `get` finds the slot gone and queues a resample as for any uncached
    /// size, and the worker makes the copy. (On `develop` the refusal was permanent: the next
    /// `get` answered `Evicted` and never queued.)
    #[test]
    fn pressure_refusal_recovers_after_a_release() {
        // Two whole 16 KiB pictures plus exactly one 1 KiB copy fit; both copies together do not.
        let per_owner = 2 * 16 * KB + 1024;
        let store = ImageStore::with_caps(per_owner, 8 * per_owner);
        let owner = 40;
        store.submit(owner, ImageKey(1), rgba(64, 64, 1));
        store.submit(owner, ImageKey(2), rgba(64, 64, 2));
        store.submit(owner, ImageKey(3), rgba(2, 2, 5)); // 16 bytes: an unrelated picture to release

        // The second copy's install pushes the caps over: key 1's copy is the victim.
        assert!(matches!(
            store.get(owner, ImageKey(1), (16, 16), None, false),
            Lookup::Pending
        ));
        assert!(matches!(
            store.get(owner, ImageKey(2), (16, 16), None, false),
            Lookup::Pending
        ));
        store.run_jobs_until_idle();
        assert!(
            matches!(
                store.get(owner, ImageKey(2), (16, 16), None, false),
                Lookup::Ready(_)
            ),
            "the second copy is installed"
        );
        let refused = store.get(owner, ImageKey(1), (16, 16), None, false);
        assert!(
            matches!(refused, Lookup::Evicted { .. }),
            "the first copy is refused under pressure, got {refused:?}"
        );
        assert_books(&store);

        // The release frees the 16 bytes, so the refused size is asked afresh...
        store.release(owner, ImageKey(3));
        assert!(
            matches!(
                store.get(owner, ImageKey(1), (16, 16), None, false),
                Lookup::Pending
            ),
            "the cleared refusal is asked afresh: the next draw queues the resample"
        );
        store.run_jobs_until_idle();
        match store.get(owner, ImageKey(1), (16, 16), None, false) {
            Lookup::Ready(img) => {
                assert_eq!((img.width, img.height), (16, 16));
            }
            other => panic!("the recovered size should answer its copy, got {other:?}"),
        }
        assert_books(&store);
    }

    /// The same recovery through a dropped owner: the pane whose picture is gone frees its bytes,
    /// and the other owner's pressure refusal comes back with a generation bump and a wake — the
    /// dropped owner's own hooks are already gone, so it is never asked to redraw.
    #[test]
    fn pressure_refusal_recovers_after_another_owner_drops() {
        // Room for two whole 16 KiB pictures and exactly one 1 KiB copy per owner; the other
        // owner holds a 4 KiB picture that goes on drop to make room for the recovered copy.
        let per_owner = 2 * 16 * KB + 1024;
        let total = 3 * 16 * KB + 1024;
        let store = Arc::new(ImageStore::with_caps(per_owner, total));
        let (keeper, victim) = (41, 42);
        let k_term = Arc::new(Mutex::new(Terminal::new(size(), 100)));
        let v_term = Arc::new(Mutex::new(Terminal::new(size(), 100)));
        let mine = Captured::default();
        let theirs = Captured::default();
        store.register_owner(keeper, mine.hooks_probing(&k_term, &store));
        store.register_owner(victim, theirs.hooks_probing(&v_term, &store));

        store.submit(keeper, ImageKey(1), rgba(64, 64, 1));
        store.submit(keeper, ImageKey(2), rgba(64, 64, 2));
        store.submit(victim, ImageKey(9), rgba(32, 32, 9)); // 4 KiB, the memory that frees
        assert_eq!(
            store.owner_bytes(keeper),
            2 * 16 * KB,
            "both pictures held before the pressure"
        );
        assert_books(&store);

        // The second copy's install pushes the keeper's own cap over (the app total stays under
        // its cap): key 1's copy is the victim.
        assert!(matches!(
            store.get(keeper, ImageKey(1), (16, 16), None, false),
            Lookup::Pending
        ));
        assert!(matches!(
            store.get(keeper, ImageKey(2), (16, 16), None, false),
            Lookup::Pending
        ));
        store.run_jobs_until_idle();
        assert!(
            matches!(
                store.get(keeper, ImageKey(2), (16, 16), None, false),
                Lookup::Ready(_)
            ),
            "the second copy is installed"
        );
        let refused = store.get(keeper, ImageKey(1), (16, 16), None, false);
        assert!(
            matches!(refused, Lookup::Evicted { .. }),
            "the first copy is refused under pressure, got {refused:?}"
        );
        assert_books(&store);
        let epoch = store.generation(keeper);
        let wakes = mine.wakes();

        // The victim's pane closes: its bytes free, and the keeper's refused size comes back.
        store.drop_owner(victim);
        assert_books(&store);
        assert_eq!(
            mine.wakes(),
            wakes + 1,
            "the keeper is asked to redraw once its refusal is cleared"
        );
        assert_eq!(
            store.generation(keeper),
            epoch + 1,
            "its generation moved with the wake: the cached frame is stale"
        );
        assert_eq!(
            theirs.wakes(),
            0,
            "the dropped owner's hooks are gone: it is not woken"
        );
        assert!(
            matches!(
                store.get(keeper, ImageKey(1), (16, 16), None, false),
                Lookup::Pending
            ),
            "the cleared refusal is asked afresh: the next draw queues the resample"
        );
        store.run_jobs_until_idle();
        assert!(
            matches!(
                store.get(keeper, ImageKey(1), (16, 16), None, false),
                Lookup::Ready(_)
            ),
            "the recovered copy is made from the freed room"
        );
        assert_books(&store);
        assert_eq!(mine.locked_hooks(), 0, "no hook ran under the store mutex");
    }

    /// A copy that cannot fit beside its own source is a GEOMETRY refusal: an unrelated release
    /// frees other memory, but it cannot make this pair fit, so the size stays refused.
    #[test]
    fn geometry_refusal_stays_after_a_release() {
        let per_owner = 16 * KB + 512; // a 16 KiB picture fits alone, never beside a copy
        let store = ImageStore::with_caps(per_owner, 8 * per_owner);
        let owner = 43;
        store.submit(owner, ImageKey(1), rgba(64, 64, 1));
        store.submit(owner, ImageKey(2), rgba(2, 2, 5)); // an unrelated 16-byte neighbour

        assert!(matches!(
            store.get(owner, ImageKey(1), (16, 16), None, false),
            Lookup::Pending
        ));
        store.run_jobs_until_idle();
        assert!(
            matches!(
                store.get(owner, ImageKey(1), (16, 16), None, false),
                Lookup::Evicted { .. }
            ),
            "the copy is refused on geometry alone"
        );
        assert_books(&store);

        // Releasing the neighbour frees 16 bytes, but the pair still cannot fit: no re-ask.
        store.release(owner, ImageKey(2));
        assert!(
            matches!(
                store.get(owner, ImageKey(1), (16, 16), None, false),
                Lookup::Evicted { .. }
            ),
            "a geometry refusal survives an unrelated release"
        );
        store.run_jobs_until_idle();
        assert!(
            store.inner.lock().jobs.is_empty(),
            "the refused size queues nothing"
        );
        assert_books(&store);
    }

    /// A release that removes nothing clears nothing: a second release of the same key, and a
    /// release of a key the owner never had, must leave pressure refusals in place and wake
    /// nobody.
    #[test]
    fn stale_release_clears_nothing() {
        let per_owner = 2 * 16 * KB + 1024;
        let store = Arc::new(ImageStore::with_caps(per_owner, 8 * per_owner));
        let (keeper, victim) = (44, 45);
        let k_term = Arc::new(Mutex::new(Terminal::new(size(), 100)));
        let mine = Captured::default();
        store.register_owner(keeper, mine.hooks_probing(&k_term, &store));

        store.submit(keeper, ImageKey(1), rgba(64, 64, 1));
        store.submit(keeper, ImageKey(2), rgba(64, 64, 2));
        store.submit(victim, ImageKey(9), rgba(32, 32, 9)); // 4 KiB: what the release frees
        assert!(matches!(
            store.get(keeper, ImageKey(1), (16, 16), None, false),
            Lookup::Pending
        ));
        assert!(matches!(
            store.get(keeper, ImageKey(2), (16, 16), None, false),
            Lookup::Pending
        ));
        store.run_jobs_until_idle();
        assert!(
            matches!(
                store.get(keeper, ImageKey(1), (16, 16), None, false),
                Lookup::Evicted { .. }
            ),
            "key 1's size is refused under pressure"
        );
        assert_books(&store);

        // The one real release frees room and the refused size is asked afresh...
        store.release(victim, ImageKey(9));
        assert!(
            matches!(
                store.get(keeper, ImageKey(1), (16, 16), None, false),
                Lookup::Pending
            ),
            "a real release clears the pressure refusal"
        );
        store.run_jobs_until_idle();
        assert!(
            matches!(
                store.get(keeper, ImageKey(1), (16, 16), None, false),
                Lookup::Ready(_)
            ),
            "key 1's copy is made again from the freed room"
        );

        // ...and a second refusal lands the same way: a new ask for the OTHER size, whose
        // install pushes the caps over and makes key 1's just-recovered copy the victim.
        store.submit(keeper, ImageKey(3), rgba(2, 2, 5)); // 16 bytes, a neighbour to release
        store.release(keeper, ImageKey(3)); // frees the 16 bytes the new ask needs
        // Key 2's copy is still cached (it never went), so ask a NEW size of key 2's picture:
        // its install needs 1 KiB and evicts the least recently shown copy — key 1's, which the
        // gets above stamped older than key 2's.
        assert!(matches!(
            store.get(keeper, ImageKey(2), (32, 32), None, false),
            Lookup::Pending
        ));
        store.run_jobs_until_idle();
        assert!(
            matches!(
                store.get(keeper, ImageKey(2), (32, 32), None, false),
                Lookup::Ready(_)
            ),
            "the new copy is installed"
        );
        assert!(
            matches!(
                store.get(keeper, ImageKey(1), (16, 16), None, false),
                Lookup::Evicted { .. }
            ),
            "key 1's size is refused under pressure again"
        );
        let wakes = mine.wakes();
        let epoch = store.generation(keeper);

        // Releasing the same key again removes nothing: the refusal stays, nobody is woken.
        store.release(victim, ImageKey(9));
        assert_eq!(mine.wakes(), wakes, "a stale release wakes nobody");
        assert_eq!(store.generation(keeper), epoch, "and it bumps nothing");
        assert!(
            matches!(
                store.get(keeper, ImageKey(1), (16, 16), None, false),
                Lookup::Evicted { .. }
            ),
            "the pressure refusal is still in place"
        );

        // And a release of a key the owner never had is just as harmless.
        store.release(keeper, ImageKey(7));
        assert_eq!(mine.wakes(), wakes, "an unknown-key release wakes nobody");
        assert_eq!(store.generation(keeper), epoch, "and it bumps nothing");
        assert!(
            matches!(
                store.get(keeper, ImageKey(1), (16, 16), None, false),
                Lookup::Evicted { .. }
            ),
            "the pressure refusal is still in place"
        );
        assert_books(&store);
    }

    /// After a clear the two placements that cannot coexist race once more: whichever copy
    /// installs displaces the other's, that size is refused again under pressure, and both
    /// settle — the bounded re-ask is what keeps the N4 loop dead.
    #[test]
    fn no_ping_pong_after_a_clear() {
        let per_owner = 2 * 16 * KB + 1024;
        let store = ImageStore::with_caps(per_owner, 8 * per_owner);
        let owner = 46;
        store.submit(owner, ImageKey(1), rgba(64, 64, 1));
        store.submit(owner, ImageKey(2), rgba(64, 64, 2));

        // Two rounds asking both sizes: they settle with one copy installed and the other refused.
        for _ in 0..2 {
            for key in [ImageKey(1), ImageKey(2)] {
                store.get(owner, key, (16, 16), None, false);
            }
            store.run_jobs_until_idle();
        }
        let settled = |key: ImageKey| {
            matches!(
                store.get(owner, key, (16, 16), None, false),
                Lookup::Evicted { .. }
            )
        };
        assert!(
            settled(ImageKey(1)) != settled(ImageKey(2)),
            "the two copies must settle, one refused"
        );

        // Which size is refused right now (before the release clears it).
        let loser = if settled(ImageKey(1)) {
            ImageKey(1)
        } else {
            ImageKey(2)
        };

        // The release that frees memory: a 3x3 neighbour of the owner.
        store.submit(owner, ImageKey(3), rgba(3, 3, 5));
        store.release(owner, ImageKey(3));

        // The clear must have happened: the refused size now answers `Pending` (its slot was
        // removed, so the ask queues a resample again) instead of `Evicted`.
        assert!(
            matches!(
                store.get(owner, loser, (16, 16), None, false),
                Lookup::Pending
            ),
            "the release cleared the pressure refusal: the next ask queues a resample"
        );

        // After the clear, both placements ask every frame again: at most two more resamples are
        // ever queued (the one the loser re-asked, and one more only if the winner's copy was
        // displaced), then they settle again.
        let mut pending_asks = 0usize;
        for _ in 0..8 {
            for key in [ImageKey(1), ImageKey(2)] {
                if matches!(
                    store.get(owner, key, (16, 16), None, false),
                    Lookup::Pending
                ) {
                    pending_asks += 1;
                }
            }
            store.run_jobs_until_idle();
        }
        assert!(
            (1..=2).contains(&pending_asks),
            "the race after a clear is bounded: {pending_asks} resamples"
        );
        assert!(
            settled(ImageKey(1)) != settled(ImageKey(2)),
            "one size ends refused again: the N4 loop cannot restart"
        );
        assert!(store.inner.lock().jobs.is_empty());
        assert_books(&store);
    }

    /// A release clears a pressure refusal by REMOVING its slot, not by queueing a resample for
    /// it: nothing touches the worker's queue, so the cleared size's copy is only made when the
    /// drawer actually asks for it — the `get` that finds no slot pushes the one `Job::Resample`
    /// and wakes the worker there. (Queueing at clear time, with no worker notification, would
    /// leave the slot `Queued` and no job running: a `Pending` the drawer waits on forever.)
    #[test]
    fn a_clear_removes_the_slot_and_only_a_get_queues_the_resample() {
        let per_owner = 2 * 16 * KB + 1024;
        let store = ImageStore::with_caps(per_owner, 8 * per_owner);
        let owner = 47;
        store.submit(owner, ImageKey(1), rgba(64, 64, 1));
        store.submit(owner, ImageKey(2), rgba(64, 64, 2));
        store.submit(owner, ImageKey(3), rgba(2, 2, 5)); // 16 bytes: the memory that frees
        // The second copy's install pushes the caps over: key 1's size is refused under pressure.
        store.get(owner, ImageKey(1), (16, 16), None, false);
        store.get(owner, ImageKey(2), (16, 16), None, false);
        store.run_jobs_until_idle();
        assert!(
            matches!(
                store.get(owner, ImageKey(1), (16, 16), None, false),
                Lookup::Evicted { .. }
            ),
            "the first copy is refused under pressure"
        );
        assert!(store.inner.lock().jobs.is_empty());

        // The release frees the 16 bytes: it clears the refusal, but it must not queue anything.
        store.release(owner, ImageKey(3));
        {
            let inner = store.inner.lock();
            let slots = &inner.owners[&owner].images[&ImageKey(1)].resamples;
            assert!(
                !slots.contains_key(&(16, 16, false)),
                "the cleared size's slot is ABSENT, not `Queued`: nothing is owed to a job"
            );
            assert!(
                inner.jobs.is_empty(),
                "a clear queues no job: the worker is only woken by the drawer's own ask"
            );
        }
        // Nothing has drawn since, so the worker must still be idle: no job has run or waited.
        assert!(store.wait_idle_for(Duration::from_secs(5)));

        // The drawer's next ask for the size takes the plain uncached path: it queues exactly one
        // resample and answers `Pending`...
        assert!(
            matches!(
                store.get(owner, ImageKey(1), (16, 16), None, false),
                Lookup::Pending
            ),
            "the cleared size is asked afresh like any uncached size"
        );
        {
            let inner = store.inner.lock();
            assert_eq!(inner.jobs.len(), 1, "the ask queued exactly one job");
            assert!(
                matches!(
                    inner.jobs.front(),
                    Some(Job::Resample {
                        key,
                        target,
                        ..
                    }) if *key == ImageKey(1) && *target == (16, 16)
                ),
                "and it is the resample of the cleared size"
            );
        }
        // ...and the worker makes the copy from the freed room.
        store.run_jobs_until_idle();
        assert!(
            matches!(
                store.get(owner, ImageKey(1), (16, 16), None, false),
                Lookup::Ready(_)
            ),
            "the copy is made on ask, not on release"
        );
        assert_books(&store);
    }

    /// A decode that finishes while no hooks are registered does not lose its answer: the key
    /// stays owed, the answer waits in the entry, and registering hands it over exactly once
    /// through the worker.
    #[test]
    fn answers_wait_for_a_registration_and_then_go_once() {
        let term = Arc::new(Mutex::new(Terminal::new(size(), 100))); // 80x24: nothing scrolls off
        let owner = term.lock().image_owner();
        let store = ImageStore::with_caps(64 * KB, 512 * KB);

        // Transmit with no hooks anywhere: the decode runs and its answer has nowhere to go.
        let feed = format!("\x1b_Gi=1,a=t,f=100;{}\x1b\\", b64(TINY_PNG));
        term.lock().feed(feed.as_bytes());
        term.lock().tick(Instant::now());
        let mut received = None;
        for event in term.lock().take_events() {
            if let TermEvent::ImageReceived { key, data } = event {
                received = Some((key, data));
            }
        }
        let (key, data) = received.expect("the transmission should have reached the terminal");
        store.submit(owner, key, data);
        store.run_jobs_until_idle();

        // Nothing was delivered: the answer is stashed and the obligation still stands.
        {
            let inner = store.inner.lock();
            let image = inner.owners[&owner].images.get(&key).unwrap();
            assert!(image.owes_reply, "the key stays owed until it is answered");
            assert!(matches!(image.pending_answer, Some(Ok(()))));
        }
        assert_books(&store);

        // Registering queues the delivery for the worker; running jobs hands it over once.
        let captured = Captured::default();
        store.register_owner(owner, captured.hooks(&term));
        assert!(
            store
                .inner
                .lock()
                .jobs
                .iter()
                .any(|job| matches!(job, Job::Answer { .. })),
            "registration should queue the owed answers for this owner",
        );
        store.run_jobs_until_idle();
        assert_eq!(
            captured.answers(),
            vec![(1u32, "OK".to_string())],
            "the waiting answer reaches the client exactly once",
        );

        // A second registration finds nothing owed left: no second answer, no second job.
        store.register_owner(owner, captured.hooks(&term));
        assert!(store.inner.lock().jobs.is_empty());
        store.run_jobs_until_idle();
        assert_eq!(captured.answers(), vec![(1u32, "OK".to_string())]);
    }

    /// The caps evict a key while its decode is already running: exactly one answer arrives for
    /// it (the refusal), the queued `Refuse` and the running decode do not both reply, and the
    /// terminal releases the key after being told.
    #[test]
    fn evicting_a_key_while_its_decode_runs_answers_exactly_once() {
        let term = Arc::new(Mutex::new(Terminal::new(size(), 100))); // 80x24: nothing scrolls off
        let owner = term.lock().image_owner();
        // A tiny PNG payload is 136 bytes: the cap fits two pending, never three.
        let per_owner = 2 * TINY_PNG.len() + 40;
        let store: &'static ImageStore =
            Box::leak(Box::new(ImageStore::with_caps(per_owner, 8 * KB)));
        let captured = Captured::default();
        store.register_owner(owner, captured.hooks(&term));

        // A hook that parks the first Encoded payload it sees until the test opens a gate. Only
        // key 1's decode can be that first: one worker thread, keys 2 and 3 queue behind it.
        let (entry_tx, entry_rx) = std::sync::mpsc::channel();
        let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();
        let gate = Arc::new(StdMutex::new(gate_rx));
        let armed = AtomicBool::new(true);
        store.set_decode_hook(Some(Arc::new(move |data| {
            if !matches!(data, ImageData::Encoded { .. }) {
                return None;
            }
            if armed.swap(false, Ordering::SeqCst) {
                entry_tx.send(()).unwrap(); // signal: the decode is inside the hook
                gate.lock().unwrap().recv().expect("the gate opens once"); // park until released
                Some(Ok(Rgba8Image {
                    width: 4,
                    height: 4,
                    pixels: tiny_rgba().into(),
                }))
            } else {
                None // later payloads take the real decoder
            }
        })));

        let transmit = |id: u32| -> ImageKey {
            let feed = format!("\x1b_Gi={id},a=t,f=100;{}\x1b\\", b64(TINY_PNG));
            term.lock().feed(feed.as_bytes());
            term.lock().tick(Instant::now());
            let mut key = None;
            for event in term.lock().take_events() {
                if let TermEvent::ImageReceived { key: k, data } = event {
                    store.submit(owner, k, data);
                    key = Some(k);
                }
            }
            key.expect("transmission should have produced an image event")
        };

        let first = transmit(1);
        store.start(); // the worker starts key 1's decode and parks in the hook
        entry_rx
            .recv_timeout(Duration::from_secs(30))
            .expect("the decode should have reached the hook");

        transmit(2);
        transmit(3); // three pending payloads: over the cap, nothing decoded yet

        // Enforce while key 1's decode runs: it is the least recently shown, so it is evicted,
        // and a `Refuse` queues beside the in-flight decode.
        store.enforce_caps(owner);
        assert_books(store);
        assert!(matches!(
            store.get(owner, first, (8, 16), None, false),
            Lookup::Evicted { .. }
        ));

        // Let the decode finish into an entry that is already `Evicted`.
        gate_tx.send(()).unwrap();
        assert!(store.wait_idle_for(Duration::from_secs(30)));
        store.set_decode_hook(None);

        let mut answers = captured.answers();
        answers.sort();
        assert_eq!(
            answers,
            vec![
                (1, "ENOMEM".to_string()),
                (2, "OK".to_string()),
                (3, "OK".to_string()),
            ],
            "the evicted key is answered exactly once — its refusal, not twice",
        );
        assert!(
            captured.saw_release(first),
            "the terminal should release the refused key"
        );
        // Only the two decoded buffers remain accounted: the evicted payload went with eviction,
        // and the useless decode result was dropped.
        assert_eq!(store.owner_bytes(owner), 2 * tiny_rgba().len());
        assert_books(store);
    }

    /// A session over its cap gives up its scrollback image before the store drops a buffer: the
    /// oldest placement becomes a placeholder line and releases, the on-screen picture stays, and
    /// the emulator's own release for the eviction is absorbed by the idempotent `release`.
    #[test]
    fn reclaim_takes_scrollback_images_first() {
        let small = GridSize {
            cols: 10,
            rows: 3,
            cell_width: 8.0,
            cell_height: 16.0,
            scale: 1.0,
        };
        let term = Arc::new(Mutex::new(Terminal::new(small, 100)));
        let owner = term.lock().image_owner();
        // Keep scrollback images: the first picture survives into history instead of becoming a
        // placeholder on its own.
        term.lock().set_images(true, true);
        let store = ImageStore::with_caps(12, 4 * KB); // two 8-byte pictures do not fit
        let captured = Captured::default();
        let weak = Arc::downgrade(&term);
        store.register_owner(owner, captured.hooks(&term));

        let transmit = |id: u32, pixels: [u8; 8]| -> (ImageKey, Vec<TermEvent>) {
            // `a=T` (not `a=t`): feathercritty only places on transmission for the capital form.
            // No `i=`: an image id is a reference of its own, and without one removing the
            // placement is what takes the image's last reference — so the emulator announces
            // the eviction with an `ImageReleased` of its own, which `release` must absorb.
            let feed = format!("\x1b_Gf=32,s=2,v=1,a=T;{}\x1b\\", b64(&pixels));
            term.lock().feed(feed.as_bytes());
            term.lock().tick(Instant::now());
            let events = term.lock().take_events();
            let key = match events.first() {
                Some(TermEvent::ImageReceived { key, data }) => {
                    assert!(matches!(data, ImageData::Rgba { .. }));
                    *key
                }
                other => panic!("expected an image event for {id}, got {other:?}"),
            };
            store.submit(
                owner,
                key,
                ImageData::Rgba {
                    width: 2,
                    height: 1,
                    pixels: pixels.to_vec(),
                },
            );
            (key, events)
        };

        let (first, first_drain) = transmit(1, [9; 8]);
        // Scroll the first picture's rows out of the viewport and into history.
        term.lock().feed(b"a\r\nb\r\nc\r\nd\r\n");
        term.lock().tick(Instant::now());
        let scroll_drain = term.lock().take_events();

        // A second picture pushes the owner over its 12-byte cap.
        let (second, second_drain) = transmit(2, [4; 8]);
        assert!(
            store.over_cap(owner),
            "the cap should be over before reclaiming"
        );
        store.wake_owners(&reclaim(&store, &weak, owner));

        // The scrollback image went first — released outright by the emulator, and then by us.
        assert_eq!(
            store.owner_bytes(owner),
            8,
            "only the on-screen picture is held"
        );
        assert!(
            matches!(
                store.get(owner, first, (8, 16), None, false),
                Lookup::Unknown
            ),
            "the evicted scrollback image was released outright",
        );
        let after_reclaim = term.lock().take_events();
        let announced = |events: &[TermEvent]| {
            events
                .iter()
                .any(|e| matches!(e, TermEvent::ImageReleased(k) if *k == first))
        };
        // The emulator's own release for the eviction rode out with the event drains (whichever
        // one it landed in is an implementation detail of when the terminal flushes events).
        let seen: Vec<&[TermEvent]> =
            vec![&first_drain, &scroll_drain, &second_drain, &after_reclaim];
        assert!(
            seen.iter().copied().any(announced),
            "the terminal should have announced the eviction's release too, saw {first_drain:?}",
        );
        // Routed like any other release: the duplicate is harmless.
        store.release(owner, first);
        assert_eq!(store.owner_bytes(owner), 8);
        assert!(
            !matches!(
                store.get(owner, second, (8, 16), None, false),
                Lookup::Evicted { .. } | Lookup::Unknown
            ),
            "the on-screen picture should not have been the one dropped",
        );
    }

    /// A pixel payload that does not match its declared dimensions is dropped: a zero-wide frame
    /// would poison the nearest-neighbour path, and a truncated one draws garbage.
    #[test]
    fn malformed_rgba_payloads_are_dropped() {
        let store = ImageStore::with_caps(64 * KB, 128 * KB);
        let owner = 9;
        let cases = [
            ImageData::Rgba {
                width: 0,
                height: 4,
                pixels: vec![7; 4],
            },
            ImageData::Rgba {
                width: 2,
                height: 0,
                pixels: Vec::new(),
            },
            // 2x2 RGBA8 needs 16 bytes: this one is short.
            ImageData::Rgba {
                width: 2,
                height: 2,
                pixels: vec![7; 12],
            },
        ];
        for (i, data) in cases.into_iter().enumerate() {
            store.submit(owner, ImageKey(1 + i as u64), data);
        }
        assert_eq!(store.owner_bytes(owner), 0);
        for i in 1..=3u64 {
            assert!(
                matches!(
                    store.get(owner, ImageKey(i), (8, 16), None, false),
                    Lookup::Unknown
                ),
                "malformed key {i} should not be stored",
            );
        }
        // A well-formed payload of the same kind still lands.
        store.submit(owner, ImageKey(4), rgba(2, 2, 5));
        assert_eq!(store.owner_bytes(owner), 16);
    }

    /// Minimal standard-alphabet base64 (padding included) for building kitty frames in tests.
    fn b64(bytes: &[u8]) -> String {
        const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let n = (u32::from(chunk[0]) << 16)
                | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
                | u32::from(*chunk.get(2).unwrap_or(&0));
            let t = |i: u32| T[i as usize] as char;
            out.push(t(n >> 18));
            out.push(t((n >> 12) & 63));
            out.push(if chunk.len() > 1 {
                t((n >> 6) & 63)
            } else {
                '='
            });
            out.push(if chunk.len() > 2 { t(n & 63) } else { '=' });
        }
        out
    }
}
