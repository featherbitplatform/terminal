//! The app-wide store of decoded images: caps, eviction, resample cache and the worker thread.
//!
//! One [`ImageStore`] serves every terminal in the process ([`store`]); tests build their own with
//! small caps through [`ImageStore::with_caps`]. Images are keyed by an *owner id* per
//! [`Terminal`] (fbterm-term cannot know the app's pane ids, and a reconnect feeds the same
//! terminal through a new session), and each owner registers [`OwnerHooks`] so finished work can
//! reach its session: the worker reports decode results to the terminal (which answers kitty
//! clients), drains the resulting events through `on_events` and wakes the pane with `wake`.
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
use parking_lot::{Condvar, Mutex};

use crate::images::decode::{self, DecodeError, DecodeHook, Rgba8Image};
use crate::images::resample::{fit_size, is_integer_multiple, resample};
use crate::images::{PER_OWNER_CAP, TOTAL_CAP};
use crate::{TermEvent, Terminal};

/// What a session registered for one terminal, so finished image work reaches it.
///
/// `term` is weak on purpose: the store must not keep a closed pane's terminal alive, and nothing
/// a hook captures may hold a strong terminal reference either (the hooks live in the store).
/// Registering again with the same owner id replaces the hooks — that is how a reconnect retargets
/// decode results to its new session while the images survive.
pub struct OwnerHooks {
    pub term: Weak<Mutex<Terminal>>,
    /// Called (on the worker thread, no store lock held) with the events a finished decode made
    /// the terminal raise: replies to programs and releases of failed images.
    pub on_events: Arc<dyn Fn(Vec<TermEvent>) + Send + Sync>,
    /// Called when new pixels became available, so the pane redraws (it is `mark_dirty`).
    pub wake: Arc<dyn Fn() + Send + Sync>,
}

/// The three hook fields of [`OwnerHooks`] cloned out together, ready to run with no store lock
/// held (what [`ImageStore::deliver`] hands an answer to).
type DeliveryHooks = (
    Weak<Mutex<Terminal>>,
    Arc<dyn Fn(Vec<TermEvent>) + Send + Sync>,
    Arc<dyn Fn() + Send + Sync>,
);

/// A resampled copy of an image at one placement size. The drawer uploads it once; a GPU handle
/// can hang off it later without changing what the cache keys on.
#[derive(Debug)]
pub struct ResampledImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl ResampledImage {
    fn bytes(&self) -> usize {
        self.pixels.len()
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
    /// The decoded buffer was dropped by the caps; the placement should draw its placeholder.
    Evicted,
    /// Never seen, or released: nothing to draw and no reason to wait.
    Unknown,
}

/// One cached resample of one image: queued, ready with its last-used stamp, or refused.
enum Slot {
    Queued,
    Ready(u64, Arc<ResampledImage>),
    /// This size has no copy and will not get one until its source changes: either the copy was
    /// dropped the moment it was installed (it could not fit beside its own source under the
    /// caps — see [`ImageStore::finish_resample`]), or another image's install-time enforcement
    /// evicted it to make room, its whole image still being held (see [`Reason::Install`]). Draws
    /// answer [`Lookup::Evicted`] for a refused size until the source image is replaced or
    /// released — spec 6.3's placeholder rule; re-asking a size enforcement already proved it
    /// cannot keep would only burn the same work every frame. Copies dropped by ordinary
    /// submit- or decode-time enforcement stay re-makeable (plain LRU).
    Refused,
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

struct Entry {
    state: State,
    /// Last time this whole image was drawn or kept alive by a draw (LRU).
    shown: u64,
    resamples: HashMap<(u32, u32), Slot>,
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

/// Buffers and jobs taken out of the store while its mutex was held. A few hundred MiB of pixel
/// buffers and up to 16 MiB of queued payloads are freed *after* the lock is released, so dropping
/// them cannot stall a draw-path `get`.
#[derive(Default)]
struct Freed {
    jobs: Vec<Job>,
    entries: Vec<Entry>,
    images: Vec<Arc<Rgba8Image>>,
    copies: Vec<Arc<ResampledImage>>,
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
    fn drain_resamples(&mut self, resamples: HashMap<(u32, u32), Slot>) {
        for (_, slot) in resamples {
            if let Slot::Ready(_, out) = slot {
                self.copies.push(out);
            }
        }
    }
}

fn new_owner() -> OwnerEntry {
    OwnerEntry {
        hooks: None,
        images: HashMap::new(),
        bytes: 0,
    }
}

struct OwnerEntry {
    hooks: Option<OwnerHooks>,
    images: HashMap<ImageKey, Entry>,
    /// Payloads waiting to decode, decoded buffers and resampled copies of this owner.
    bytes: usize,
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
    pub fn drop_owner(&self, owner: u64) {
        let mut freed = Freed::default();
        // The session hooks die with the owner; they are dropped after the lock too (they hold the
        // session's senders, never the terminal, but nothing belongs inside a short section).
        let mut hooks = Vec::new();
        {
            let mut inner = self.inner.lock();
            if let Some(entry) = inner.owners.remove(&owner) {
                inner.total -= entry.bytes;
                hooks.extend(entry.hooks);
                for (_, image) in entry.images {
                    freed.drain_entry(image);
                }
            }
            Freed::take_jobs(&mut inner, |job| job_owner(job) != owner, &mut freed);
            Self::check_idle(&inner, &self.idle);
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
                pixels,
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
        let payload = match &data {
            ImageData::Encoded { bytes, .. } => bytes.len(),
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

    /// Forget one image entirely (its [`TermEvent::ImageReleased`]). Idempotent: the terminal also
    /// releases keys that scrollback eviction or a failed decode already dropped from the store,
    /// and a second release of the same key must stay harmless. Nothing is answered for a released
    /// key: the terminal has forgotten it, client included.
    pub fn release(&self, owner: u64, key: ImageKey) {
        let mut freed = Freed::default();
        {
            let mut inner = self.inner.lock();
            if let Some(entry) = inner.owners.get_mut(&owner)
                && let Some(image) = entry.images.remove(&key)
            {
                let gone = image.accounted();
                entry.bytes -= gone;
                inner.total -= gone;
                freed.drain_entry(image);
            }
            Freed::take_jobs(&mut inner, |job| !job_is(job, owner, key), &mut freed);
            Self::check_idle(&inner, &self.idle);
        }
    }

    /// Ask for a resample of `key` covering `span` cells of `cell_px` logical pixels at the
    /// display scale whose bits are `scale_bits`, fitted inside that box with its aspect kept.
    ///
    /// A size not cached yet is queued on the worker and answers [`Lookup::Pending`]: nothing is
    /// ever resampled synchronously here, because `get` runs on the draw path. When the resample
    /// finishes, the owner's `wake` fires and a later `get` returns it.
    pub fn get(
        &self,
        owner: u64,
        key: ImageKey,
        cell_px: (u32, u32),
        span: (u32, u32),
        scale_bits: u32,
    ) -> Lookup {
        let box_px = placement_box(cell_px, span, scale_bits);
        let mut inner = self.inner.lock();
        let Some(owner_entry) = inner.owners.get_mut(&owner) else {
            return Lookup::Unknown;
        };
        let Some(image) = owner_entry.images.get_mut(&key) else {
            return Lookup::Unknown;
        };
        let src = match &image.state {
            State::Ready(src) => src.clone(),
            State::Pending(_) => return Lookup::Pending,
            State::Evicted => return Lookup::Evicted,
        };
        let target = fit_capped(src.width, src.height, box_px.0, box_px.1);
        image.shown = self.next_stamp();
        match image.resamples.get_mut(&target) {
            Some(Slot::Ready(stamp, out)) => {
                *stamp = self.next_stamp();
                return Lookup::Ready(out.clone());
            }
            // already queued with this draw or the last one
            Some(Slot::Queued) => return Lookup::Pending,
            // a copy of this size provably cannot fit beside its source (see `finish_resample`)
            Some(Slot::Refused) => return Lookup::Evicted,
            None => {}
        }
        let integer = is_integer_multiple(src.width, src.height, target.0, target.1);
        image.resamples.insert(target, Slot::Queued);
        // This picture's other cached sizes stay: a drawer that later comes back to one of them
        // should not have to re-make it, and memory is bounded by the LRU plus cap enforcement at
        // install time (see `finish_resample`), not by dropping siblings here.
        inner.jobs.push_back(Job::Resample {
            owner,
            key,
            target,
            integer,
            src,
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

    /// Drop buffers until the caps hold again (the step after scrollback eviction ran dry or was
    /// impossible): least recently shown first — this owner's resampled copies, then its whole
    /// images, then other owners' resamples, then their whole images. Whole images become
    /// [`Lookup::Evicted`] for later draws; released keys stay gone entirely. Any refusal this
    /// queues for an evicted owed key is followed by a worker wake (see [`Job::Refuse`]).
    pub fn enforce_caps(&self, owner: u64) {
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
                integer,
                src,
            } => {
                let out = resample(&src, target, integer);
                self.finish_resample(owner, key, target, out);
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
                                Ok(img) => State::Ready(Arc::new(img)),
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
                    .map(|h| (h.term.clone(), h.on_events.clone(), h.wake.clone()))
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
                    .map(|h| (h.term.clone(), h.on_events.clone(), h.wake.clone()));
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
                    .map(|h| (h.term.clone(), h.on_events.clone(), h.wake.clone()));
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
    /// then the terminal is locked briefly to answer, then the hooks run with nothing held. A
    /// dropped terminal means nobody waits, so it gets nothing.
    fn deliver(
        &self,
        owner: u64,
        key: ImageKey,
        answer: Result<(), ImageDecodeError>,
        hooks: DeliveryHooks,
    ) {
        let (term, on_events, wake) = hooks;
        reclaim(self, &term, owner);
        let Some(term) = term.upgrade() else { return };
        let events = {
            let mut guard = term.lock();
            // Answers the client that was waiting (`OK`, `EBADPNG` or `ENOMEM`) and, on failure,
            // removes the image and its placements (raising their releases).
            guard.image_decoded(key, answer);
            guard.take_events()
        };
        drop(term);
        on_events(events);
        wake();
    }

    /// Install a finished resample and wake the pane so it redraws with the new copy.
    fn finish_resample(&self, owner: u64, key: ImageKey, target: (u32, u32), out: Rgba8Image) {
        // Declared before the lock guard so everything enforcement takes goes after the mutex.
        let mut freed = Freed::default();
        let wake = {
            let mut inner = self.inner.lock();
            // Only install onto the exact queue slot this job filled: if the source was evicted
            // or released meanwhile, its Arc in this job is what kept it alive, and now goes.
            let queued = inner
                .owners
                .get(&owner)
                .and_then(|e| e.images.get(&key))
                .is_some_and(|image| {
                    matches!(image.state, State::Ready(_))
                        && matches!(image.resamples.get(&target), Some(Slot::Queued))
                });
            if !queued {
                return;
            }
            let out = Arc::new(ResampledImage {
                width: out.width,
                height: out.height,
                pixels: out.pixels,
            });
            let stamp = self.next_stamp();
            let bytes = out.bytes();
            // A copy that cannot fit beside its own source under the tightest cap is refused up
            // front: no enforcement pass could free more than "everything except this pair", and
            // evicting other images for a copy that comes back out anyway is pure collateral.
            let cannot_fit = inner
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
                    image.resamples.insert(target, Slot::Refused);
                }
                freed.copies.push(out); // the made buffer goes after the mutex, untouched
                return; // and nothing is woken: no draw could use what was refused
            }
            if let Some(entry) = inner.owners.get_mut(&owner)
                && let Some(image) = entry.images.get_mut(&key)
            {
                image.resamples.insert(target, Slot::Ready(stamp, out));
                image.resample_bytes += bytes;
                entry.bytes += bytes;
            }
            inner.total += bytes;
            // Resampled bytes count against the caps too, so they are enforced here — with this
            // copy and its source image protected: what goes first is an older copy or an older
            // picture, never the one just asked for. Every other copy this drops is marked
            // refused (see [`Reason::Install`]): a displaced on-screen size becomes its
            // placeholder instead of resampling once per frame beside the new copy. If only the
            // protected picture is left and a cap is still over — unreachable with the up-front
            // check, which would have refused it before evicting anything for it — the copy comes
            // back out and the size is refused all the same. A refusal this queues needs no wake
            // of its own: this job's worker loop picks up whatever is next when it returns.
            let (satisfied, _queued) = Self::enforce_inner(
                &mut inner,
                owner,
                Reason::Install(Protect { owner, key, target }),
                &mut freed,
            );
            if satisfied {
                inner
                    .owners
                    .get(&owner)
                    .and_then(|e| e.hooks.as_ref().map(|h| h.wake.clone()))
            } else {
                let mut refused_bytes = 0usize;
                if let Some(entry) = inner.owners.get_mut(&owner)
                    && let Some(image) = entry.images.get_mut(&key)
                    && let Some(Slot::Ready(_, copy)) = image.resamples.remove(&target)
                {
                    let gone = copy.bytes();
                    entry.bytes -= gone;
                    image.resample_bytes -= gone;
                    freed.copies.push(copy);
                    image.resamples.insert(target, Slot::Refused);
                    refused_bytes = gone;
                }
                inner.total -= refused_bytes;
                None
            }
        };
        if let Some(wake) = wake {
            wake();
        }
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
pub fn reclaim(store: &ImageStore, term: &Weak<Mutex<Terminal>>, owner: u64) {
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
    store.enforce_caps(owner);
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

/// The pixel box of a placement: `span` cells at `cell_px` logical pixels, rounded to whole
/// physical pixels at the display scale whose bits are `scale_bits`. Saturating: a hostile span
/// cannot overflow into a tiny box.
fn placement_box(cell_px: (u32, u32), span: (u32, u32), scale_bits: u32) -> (u32, u32) {
    let scale = f32::from_bits(scale_bits);
    let cell = [cell_px.0, cell_px.1];
    let span = [span.0, span.1];
    let mut out = [1u64, 1];
    for axis in 0..2 {
        let phys = if scale.is_finite() && scale > 0.0 {
            (f64::from(cell[axis].max(1)) * f64::from(scale))
                .round()
                .max(1.0) as u64
        } else {
            u64::from(cell[axis].max(1))
        };
        out[axis] = phys
            .saturating_mul(u64::from(span[axis].max(1)))
            .min(u64::from(u32::MAX));
    }
    (out[0] as u32, out[1] as u32)
}

/// [`fit_size`] with the store's further bound: a fitted copy never exceeds one
/// [`MAX_IMAGE_BYTES`](feathercritty::graphics::limits::MAX_IMAGE_BYTES) RGBA8 frame, so even an
/// absurd placement request cannot ask for a buffer bigger than the per-image cap.
fn fit_capped(src_w: u32, src_h: u32, box_w: u32, box_h: u32) -> (u32, u32) {
    use feathercritty::graphics::limits::MAX_IMAGE_BYTES;
    let (mut w, mut h) = fit_size(src_w, src_h, box_w, box_h);
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
/// copy at exactly `target` (the other copies of the same picture stay evictable — they are what
/// a moved drawer no longer needs; see [`ImageStore::enforce_inner`]).
#[derive(Clone, Copy)]
struct Protect {
    owner: u64,
    key: ImageKey,
    target: (u32, u32),
}

/// Why an enforcement pass runs, and what it is allowed to do about copies.
///
/// [`Reason::Routine`] is reclaim after a submit or a decode completion: plain LRU, and evicted
/// copies stay re-makeable — the drawer may ask for them again and get them.
/// [`Reason::Install`] names the copy being put in place: that copy and its source are skipped,
/// and every *other* copy this pass drops is marked [`Slot::Refused`]. A copy of a still-held
/// image displaced by an install must become its placeholder rather than resample once per frame
/// — otherwise two placements whose copies cannot coexist evict each other forever (the loop the
/// round-3 review calls N4).
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
/// plain LRU step), and an install-time eviction marks the losing size [`Slot::Refused`]: a copy
/// displaced to make room for a fresh install must not be re-made every frame. Returns whether
/// anything went.
fn evict_resample(
    inner: &mut Inner,
    trigger: u64,
    scope: Scope,
    reason: Reason,
    freed: &mut Freed,
) -> bool {
    let mut best: Option<(u64, u64, ImageKey, (u32, u32))> = None; // (stamp, owner, key, target)
    for (owner, entry) in &inner.owners {
        if !covers(*owner, trigger, scope) {
            continue;
        }
        for (key, image) in &entry.images {
            for (target, slot) in &image.resamples {
                let Slot::Ready(stamp, _) = slot else {
                    continue;
                };
                if matches!(reason, Reason::Install(p)
                    if p.owner == *owner && p.key == *key && p.target == *target)
                {
                    continue; // the copy enforcement was just asked to keep
                }
                if best.as_ref().is_none_or(|(s, ..)| *stamp < *s) {
                    best = Some((*stamp, *owner, *key, *target));
                }
            }
        }
    }
    let Some((_, owner, key, target)) = best else {
        return false;
    };
    // The scan borrowed; re-look the winner up mutably (it returned plain values).
    let entry = inner.owners.get_mut(&owner).unwrap();
    let image = entry.images.get_mut(&key).unwrap();
    if let Some(Slot::Ready(_, out)) = image.resamples.remove(&target) {
        let bytes = out.bytes();
        entry.bytes -= bytes;
        image.resample_bytes -= bytes;
        freed.copies.push(out); // dropped after the mutex goes
        if matches!(reason, Reason::Install(..)) {
            // The source is still held (only copies are evicted here), so this size draws its
            // placeholder until the source changes — refusing it stops the ask-evict-ask loop.
            image.resamples.insert(target, Slot::Refused);
        }
        inner.total -= bytes;
    }
    true
}

/// Drop the least recently shown whole image in scope: its buffer and every resampled copy of it
/// (the source is gone — any refused sizes go with it, so they are asked afresh if the key is
/// re-submitted). A pending decode counts as a whole image too, and evicting one must not strand
/// its answer: its queued jobs are purged and a [`Job::Refuse`] takes their place (if the decode
/// was already running, both exist and the exactly-once rule on [`Job`] picks one) — `queued_job`
/// reports that push so the caller can wake the worker. The image `Reason::Install` names is
/// skipped in the scan.
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

    /// Events and wakes captured through the owner hooks.
    #[derive(Clone, Default)]
    struct Captured(
        Arc<StdMutex<Vec<TermEvent>>>,
        Arc<std::sync::atomic::AtomicUsize>,
    );

    impl Captured {
        fn hooks(&self, term: &Arc<Mutex<Terminal>>) -> OwnerHooks {
            let sink = self.0.clone();
            let wakes = self.1.clone();
            OwnerHooks {
                term: Arc::downgrade(term),
                on_events: Arc::new(move |events| sink.lock().unwrap().extend(events)),
                wake: Arc::new(move || {
                    wakes.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }),
            }
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
            store.get(owner, ImageKey(1), (8, 16), (1, 1), 1.0f32.to_bits()),
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
                match store.get(owner, key, (8, 16), (1, 1), 1.0f32.to_bits()) {
                    Lookup::Pending | Lookup::Ready(_) | Lookup::Evicted => {}
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
        let scale = 1.0f32.to_bits();
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
                        store.get(owner, ImageKey(k), (8, 16), (1, 1), scale),
                        Lookup::Evicted
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
                store.get(owner, ImageKey(k), (8, 16), (2, 2), scale),
                Lookup::Evicted
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
        let scale = 1.0f32.to_bits();
        for i in 1..=2u64 {
            store.submit(owner, ImageKey(i), rgba(64, 64, 7)); // 16 KiB each
        }
        assert_eq!(store.owner_bytes(owner), 32 * KB);

        // Cache a 32x32 copy of key 1 (4 KiB): the owner sits at 36 KiB, still under its cap.
        assert!(matches!(
            store.get(owner, ImageKey(1), (16, 16), (2, 2), scale),
            Lookup::Pending
        ));
        store.run_jobs_until_idle();
        assert!(matches!(
            store.get(owner, ImageKey(1), (16, 16), (2, 2), scale),
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
                    store.get(owner, ImageKey(k), (8, 16), (1, 1), scale),
                    Lookup::Evicted
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
            store.get(owner, ImageKey(1), (8, 16), (1, 1), scale),
            Lookup::Evicted
        ));
        for k in [2u64, 3, 4] {
            assert!(
                !matches!(
                    store.get(owner, ImageKey(k), (8, 16), (1, 1), scale),
                    Lookup::Evicted
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
            store.get(owner, ImageKey(1), (8, 16), (1, 1), 1.0f32.to_bits()),
            Lookup::Unknown
        ));

        // A second drop is harmless.
        store.drop_owner(owner);
    }

    /// The full decode loop against a real terminal: a corrupt payload answers the kitty client
    /// `EBADPNG` through the owner's `on_events`, and the worker keeps serving the next job. A
    /// decoder that panics (injected hook) is contained the same way.
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
            store.get(owner, key, (8, 16), (1, 1), 1.0f32.to_bits()),
            Lookup::Evicted
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
        let scale = 1.0f32.to_bits();
        assert!(matches!(
            store.get(owner, key3, (8, 16), (1, 1), scale),
            Lookup::Pending
        ));
        store.run_jobs_until_idle();
        match store.get(owner, key3, (8, 16), (1, 1), scale) {
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
        let scale = 2.0f32.to_bits();
        match store.get(owner, ImageKey(50), (8, 16), (1, 1), scale) {
            Lookup::Pending => {
                assert!(store.wait_idle_for(Duration::from_secs(30)));
                match store.get(owner, ImageKey(50), (8, 16), (1, 1), scale) {
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

    /// The identity resample (box exactly the source size) returns the fixture's pixels exactly.
    #[test]
    fn decoded_buffers_survive_the_identity_resample() {
        let store = ImageStore::with_caps(64 * KB, 128 * KB);
        store.submit(1, ImageKey(70), png(TINY_PNG));
        store.run_jobs_until_idle();
        let scale = 1.0f32.to_bits();
        assert!(matches!(
            store.get(1, ImageKey(70), (4, 4), (1, 1), scale),
            Lookup::Pending
        ));
        store.run_jobs_until_idle();
        match store.get(1, ImageKey(70), (4, 4), (1, 1), scale) {
            Lookup::Ready(img) => assert_eq!(img.pixels, tiny_rgba()),
            other => panic!("expected the identity resample, got {other:?}"),
        }
    }

    #[test]
    fn placement_boxes_round_physical_pixels_without_overflow() {
        let scale = 2.0f32.to_bits();
        // 8x16 logical, scale 2, a 3x4 span: whole physical pixels.
        assert_eq!(placement_box((8, 16), (3, 4), scale), (48, 128));
        // A fractional scale rounds each cell to the nearest whole pixel (half away from zero).
        let scale = 1.5f32.to_bits();
        assert_eq!(placement_box((7, 13), (2, 2), scale), (22, 40));
        // A missing font metric still never yields a zero-wide box.
        assert_eq!(placement_box((0, 0), (0, 0), 1.0f32.to_bits()), (1, 1));
        // A hostile span saturates instead of wrapping.
        assert_eq!(
            placement_box((8, 16), (u32::MAX, u32::MAX), scale).0,
            u32::MAX
        );
    }

    #[test]
    fn decoded_fits_are_capped_at_one_frame() {
        // A huge box on a large source: the copy never exceeds one 64 MiB RGBA8 frame.
        let fitted = fit_capped(10_000, 10_000, u32::MAX, u32::MAX);
        assert!((fitted.0 as u64) * u64::from(fitted.1) * 4 <= 64 * 1024 * 1024);
        assert!(fitted.0 >= 1 && fitted.1 >= 1);
        // A normal box is unaffected: exact scales pass through.
        assert_eq!(fit_capped(8, 8, 16, 16), (16, 16));
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
                    // Exactly what a session does after a submit, through the shared helper.
                    reclaim(&store, &weak, owner);
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
                    store.get(owner, *key, (8, 16), (1, 1), 1.0f32.to_bits()),
                    Lookup::Evicted
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
                    store.get(owner, *key, (8, 16), (1, 1), 1.0f32.to_bits()),
                    Lookup::Evicted
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
        let scale = 1.0f32.to_bits();
        store.submit(owner, ImageKey(1), rgba(64, 64, 7));
        assert_eq!(store.owner_bytes(owner), 16 * KB);

        for cells in [(16u32, 16u32), (12, 12), (8, 8), (20, 20), (4, 4)] {
            if matches!(
                store.get(owner, ImageKey(1), (cells.0, cells.1), (1, 1), scale),
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
                store.get(owner, ImageKey(1), (4, 4), (1, 1), scale),
                Lookup::Ready(_)
            ),
            "the last-asked size should be the cached one",
        );
        let asked_first = store.get(owner, ImageKey(1), (16, 16), (1, 1), scale);
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
        let scale = 1.0f32.to_bits();
        store.submit(owner, ImageKey(1), rgba(64, 64, 7)); // 16 KiB source
        store.start();

        let mut pending_asks = 0;
        for round in 0..12 {
            for cell_px in [(16u32, 16u32), (32, 32)] {
                if matches!(
                    store.get(owner, ImageKey(1), cell_px, (1, 1), scale),
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
                        store.get(owner, ImageKey(1), cell_px, (1, 1), scale),
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
        let scale = 1.0f32.to_bits();
        for i in 1..=3u64 {
            store.submit(owner, ImageKey(i), rgba(64, 64, 3)); // key 3 is the newest shown
        }
        assert_eq!(store.owner_bytes(owner), per_owner);
        assert_books(&store);

        // A 16x16 copy (1 KiB) of the most recently shown picture pushes the owner over its cap.
        assert!(matches!(
            store.get(owner, ImageKey(3), (16, 16), (1, 1), scale),
            Lookup::Pending
        ));
        store.run_jobs_until_idle();
        assert_books(&store);

        // The copy is there — key 1, the least recently shown picture, paid for it. Both the new
        // copy and its source were protected from this enforcement, so neither could be the victim:
        // exactly 1 KiB went over, and only a whole picture frees that much.
        assert!(matches!(
            store.get(owner, ImageKey(3), (16, 16), (1, 1), scale),
            Lookup::Ready(_)
        ));
        assert!(matches!(
            store.get(owner, ImageKey(1), (8, 16), (1, 1), scale),
            Lookup::Evicted
        ));
        // One more ask: still Ready, and nothing queued behind it — no install, no eviction churn.
        assert!(matches!(
            store.get(owner, ImageKey(3), (16, 16), (1, 1), scale),
            Lookup::Ready(_)
        ));
        assert!(store.inner.lock().jobs.is_empty());
        assert!(store.owner_bytes(owner) <= per_owner);
        assert_books(&store);
    }

    /// A copy that cannot fit beside its own source is refused — up front when geometry alone
    /// proves it (see [`ImageStore::finish_resample`]), or by dropping back out when enforcement
    /// gets only as far as "nothing but the protected picture" — with no `wake` (a redraw would
    /// only queue it again), and the refusal remembered for that `(key, target)`: every later draw
    /// answers `Evicted` without queueing, until the source image is replaced or released. That
    /// memory is the chosen loop guard: re-asking a size that provably cannot fit would burn the
    /// same work forever.
    #[test]
    fn a_copy_that_cannot_fit_is_refused_once_and_stays_refused() {
        let per_owner = 16 * KB + 512; // room for a 16 KiB picture, not for a copy beside it
        let store = ImageStore::with_caps(per_owner, 8 * per_owner);
        let owner = 14;
        let scale = 1.0f32.to_bits();
        let term = Arc::new(Mutex::new(Terminal::new(size(), 100)));
        let captured = Captured::default();
        store.register_owner(owner, captured.hooks(&term));

        store.submit(owner, ImageKey(1), rgba(64, 64, 1)); // 16 KiB: fits alone, under the cap
        assert_books(&store);

        // A 16x16 copy (1 KiB): source plus copy exceed the cap on geometry alone, so it is
        // refused outright.
        assert!(matches!(
            store.get(owner, ImageKey(1), (16, 16), (1, 1), scale),
            Lookup::Pending
        ));
        store.run_jobs_until_idle(); // the only `Pending` this ask will ever produce
        assert_books(&store);

        for _ in 0..4 {
            assert!(
                matches!(
                    store.get(owner, ImageKey(1), (16, 16), (1, 1), scale),
                    Lookup::Evicted
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
            0,
            "an unsatisfiable install must not wake a redraw"
        );
        assert_eq!(store.owner_bytes(owner), 16 * KB);
        assert!(store.owner_bytes(owner) <= per_owner);

        // The refusal belongs to the source entry: a new picture under the same key is asked
        // afresh (and refused afresh — the geometry has not changed).
        store.submit(owner, ImageKey(1), rgba(64, 64, 2));
        assert!(matches!(
            store.get(owner, ImageKey(1), (16, 16), (1, 1), scale),
            Lookup::Pending
        ));
        store.run_jobs_until_idle();
        assert!(matches!(
            store.get(owner, ImageKey(1), (16, 16), (1, 1), scale),
            Lookup::Evicted
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
        let scale = 1.0f32.to_bits();
        let term = Arc::new(Mutex::new(Terminal::new(size(), 100)));
        let captured = Captured::default();
        store.register_owner(owner, captured.hooks(&term));

        store.submit(owner, ImageKey(1), rgba(64, 64, 1)); // 16 KiB source
        store.submit(owner, ImageKey(2), rgba(8, 8, 3)); // a 256-byte neighbour
        assert_eq!(store.owner_bytes(owner), 16 * KB + 256);
        assert_books(&store);

        // 16 KiB + 1 KiB exceeds the owner's cap on its own: refused at once, nothing evicted.
        assert!(matches!(
            store.get(owner, ImageKey(1), (16, 16), (1, 1), scale),
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
            store.get(owner, ImageKey(1), (16, 16), (1, 1), scale),
            Lookup::Evicted
        ));
        assert_eq!(captured.wakes(), 0, "a refused install wakes nothing");
        assert_eq!(
            store.owner_bytes(owner),
            16 * KB + 256,
            "byte counts untouched"
        );
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
        let scale = 1.0f32.to_bits();
        store.submit(owner, ImageKey(1), rgba(64, 64, 1));
        store.submit(owner, ImageKey(2), rgba(64, 64, 2));
        store.start();

        let mut pending_asks = 0;
        for round in 0..12 {
            for key in [ImageKey(1), ImageKey(2)] {
                if matches!(
                    store.get(owner, key, (16, 16), (1, 1), scale),
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
                store.get(owner, ImageKey(2), (16, 16), (1, 1), scale),
                Lookup::Ready(_)
            ),
            "the newest copy stays",
        );
        assert!(
            matches!(
                store.get(owner, ImageKey(1), (16, 16), (1, 1), scale),
                Lookup::Evicted
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
                    pixels: tiny_rgba(),
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
            store.get(owner, first, (8, 16), (1, 1), 1.0f32.to_bits()),
            Lookup::Evicted
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
        reclaim(&store, &weak, owner);

        // The scrollback image went first — released outright by the emulator, and then by us.
        assert_eq!(
            store.owner_bytes(owner),
            8,
            "only the on-screen picture is held"
        );
        assert!(
            matches!(
                store.get(owner, first, (8, 16), (1, 1), 1.0f32.to_bits()),
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
                store.get(owner, second, (8, 16), (1, 1), 1.0f32.to_bits()),
                Lookup::Evicted | Lookup::Unknown
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
                    store.get(owner, ImageKey(i), (8, 16), (1, 1), 1.0f32.to_bits()),
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
