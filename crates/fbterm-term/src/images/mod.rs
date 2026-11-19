//! Decoding, storing and bounding the pictures programs send.
//!
//! The emulator parses the graphics protocols and hands every image payload to the host as
//! [`crate::TermEvent::ImageReceived`]; this module owns what happens next: decoding the container
//! formats off the UI thread ([`decode`]), keeping decoded pixel buffers under per-terminal and
//! app-wide memory caps with least-recently-shown eviction, and resampled copies per placement
//! size. The store answers the drawer through `ImageStore::get` and tells programs' decode results
//! back to the emulator, which answers the clients.

pub mod decode;
pub mod resample;
mod store;

pub use decode::{DecodeError, Rgba8Image, decode};
pub use resample::{fit_size, is_integer_multiple, resample};
// `src_area` too: the drawer measures a picture the way the store does before asking for it
// (findings C2 and I1), so the two can never disagree about what a placement shows.
pub(crate) use store::src_area;
pub use store::{ImageStore, Lookup, OwnerHooks, ResampledImage, reclaim, store};

// The emulator's image vocabulary, re-exported so hosts route `TermEvent::ImageReceived` and
// answer decode results without depending on feathercritty directly.
pub use feathercritty::event::{ImageData, ImageDecodeError, ImageFormat, ImageKey};

/// Decoded image bytes one terminal may hold (spec 6.3: 256 MiB per pane).
pub const PER_OWNER_CAP: usize = 256 * 1024 * 1024;

/// Decoded image bytes the whole app may hold (spec 6.3: 512 MiB).
pub const TOTAL_CAP: usize = 512 * 1024 * 1024;
