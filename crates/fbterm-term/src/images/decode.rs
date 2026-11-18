//! Decoding the container formats the terminal hands over into RGBA8 pixels.
//!
//! `png`, `zune-jpeg` and `gif` are fed from memory only — a remote program never makes fbterm
//! touch the filesystem (spec anti-goal). Every decoder reads just enough of the payload to learn
//! its dimensions, and a header claiming more than [`MAX_IMAGE_SIDE`] pixels on a side or one
//! RGBA8 frame over [`MAX_IMAGE_BYTES`] is refused with [`DecodeError::TooLarge`] before any pixel
//! allocation. A decoder that panics (these are data from across the network) is caught: the
//! image becomes [`DecodeError::Corrupt`] and nothing else is affected.
//!
//! The GIF decoder takes the first frame only; animation is a planned future feature (spec 3).

use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use bytes::Bytes;
use feathercritty::event::{ImageData, ImageDecodeError, ImageFormat};
use feathercritty::graphics::limits::{MAX_IMAGE_BYTES, MAX_IMAGE_SIDE};

/// One decoded image: straight (non-premultiplied) 8-bit RGBA, `pixels.len()` equal to
/// `width * height * 4`.
///
/// The buffer is reference counted (`bytes::Bytes`) rather than owned so that a copy at the
/// picture's own size can hand out the source's allocation instead of filtering an identical
/// second frame: a picture drawn both 1:1 and scaled keeps one RGBA8 buffer (finding P4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rgba8Image {
    pub width: u32,
    pub height: u32,
    pub pixels: Bytes,
}

impl Rgba8Image {
    /// Bytes this frame occupies: the unit the store's caps count.
    pub fn bytes(&self) -> usize {
        self.pixels.len()
    }
}

/// Why a payload is not usable as an image. Maps onto the emulator's reply codes: `Corrupt`
/// answers `EBADPNG`, `TooLarge` answers `ENOMEM` (feathercritty change 16).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// The payload is not a valid image in its format.
    Corrupt,
    /// Decoding would exceed the terminal's image size limits.
    TooLarge,
}

impl From<DecodeError> for ImageDecodeError {
    fn from(e: DecodeError) -> Self {
        match e {
            DecodeError::Corrupt => ImageDecodeError::Corrupt,
            DecodeError::TooLarge => ImageDecodeError::TooLarge,
        }
    }
}

/// A test's stand-in for the real decoders (see [`decode_with`]). Returning `None` lets the
/// payload through to the decoder below.
pub(crate) type DecodeHook =
    Arc<dyn Fn(&ImageData) -> Option<Result<Rgba8Image, DecodeError>> + Send + Sync>;

/// Decode one image payload the terminal handed over.
pub fn decode(data: ImageData) -> Result<Rgba8Image, DecodeError> {
    decode_with(&data, None)
}

/// [`decode`] with an optional test hook consulted before the real decoders run.
///
/// The whole attempt runs inside `catch_unwind`: a decoder panic on hostile input becomes
/// [`DecodeError::Corrupt`] instead of taking the worker thread down with it.
pub(crate) fn decode_with(
    data: &ImageData,
    hook: Option<&DecodeHook>,
) -> Result<Rgba8Image, DecodeError> {
    let attempt = std::panic::catch_unwind(AssertUnwindSafe(|| {
        if let Some(hook) = hook
            && let Some(result) = hook(data)
        {
            return result;
        }
        decode_inner(data)
    }));
    match attempt {
        Ok(result) => result,
        Err(_) => {
            tracing::warn!("an image decoder panicked on a payload; it is refused as corrupt");
            Err(DecodeError::Corrupt)
        }
    }
}

/// A side over `MAX_IMAGE_SIDE`, or one RGBA8 frame over `MAX_IMAGE_BYTES`. Checked from the
/// header alone, so nothing has been allocated yet.
fn check_dims(width: u32, height: u32) -> Result<(), DecodeError> {
    if width > MAX_IMAGE_SIDE || height > MAX_IMAGE_SIDE {
        return Err(DecodeError::TooLarge);
    }
    let frame = u64::from(width) * u64::from(height) * 4;
    if frame > MAX_IMAGE_BYTES {
        return Err(DecodeError::TooLarge);
    }
    Ok(())
}

fn decode_inner(data: &ImageData) -> Result<Rgba8Image, DecodeError> {
    match data {
        // Pixels the terminal decoded itself (sixel output, raw kitty RGB/RGBA): they arrive
        // straight and need no answer back.
        ImageData::Rgba {
            width,
            height,
            pixels,
        } => {
            if pixels.len() != usize::try_from(*width * *height).unwrap_or(usize::MAX) * 4 {
                return Err(DecodeError::Corrupt);
            }
            Ok(Rgba8Image {
                width: *width,
                height: *height,
                pixels: pixels.clone().into(),
            })
        }
        ImageData::Encoded { format, bytes, .. } => match format {
            ImageFormat::Png => decode_png(bytes),
            ImageFormat::Jpeg => decode_jpeg(bytes),
            ImageFormat::Gif => decode_gif(bytes),
        },
    }
}

/// The `png` crate's errors, translated. Its `LimitsExceeded` means the decoder's own default
/// 64 MiB allocation budget ran out — reachable when a well-formed stream carries a non-IDAT
/// chunk bigger than that: IDAT is inflated as it arrives and bypasses the accounting, but any
/// other chunk must be buffered whole, and only the decoder's internal accounting can see such a
/// chunk (our header checks pass on it). That is "over our size limits", not corruption: the
/// input is valid, just bigger than anything we would hold. Everything else says the bytes are
/// not to be trusted.
fn map_png_error(e: png::DecodingError) -> DecodeError {
    match e {
        png::DecodingError::LimitsExceeded => DecodeError::TooLarge,
        _ => DecodeError::Corrupt,
    }
}

/// PNG, decoded to RGBA8 through the `png` crate's own transforms: `ALPHA` expands palettes,
/// gray and RGB sources to include an alpha channel, and `STRIP_16` takes the high byte of 16-bit
/// samples, so every accepted frame is exactly `width * height * 4` bytes. The first frame of an
/// APNG stands in for the animation.
fn decode_png(bytes: &[u8]) -> Result<Rgba8Image, DecodeError> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    // `read_header_info` reads only the signature and IHDR: dimensions are known here, and no
    // pixel memory has been touched yet.
    let (width, height) = decoder
        .read_header_info()
        .map_err(map_png_error)
        .map(|info| (info.width, info.height))?;
    check_dims(width, height)?;

    decoder.set_transformations(png::Transformations::ALPHA | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(map_png_error)?;
    // An APNG's first frame can declare dimensions the IHDR did not; re-check against what the
    // buffer will actually be before allocating it.
    let out_size = reader.output_buffer_size();
    if out_size > MAX_IMAGE_BYTES as usize {
        return Err(DecodeError::TooLarge);
    }
    let mut buf = vec![0u8; out_size];
    let info = reader.next_frame(&mut buf).map_err(map_png_error)?;
    buf.truncate(info.buffer_size());
    if buf.len() != usize::try_from(width * height).unwrap_or(usize::MAX) * 4 {
        return Err(DecodeError::Corrupt);
    }
    Ok(Rgba8Image {
        width,
        height,
        pixels: buf.into(),
    })
}

/// JPEG through `zune-jpeg`, asking it for RGBA output so no conversion pass is needed here.
fn decode_jpeg(bytes: &[u8]) -> Result<Rgba8Image, DecodeError> {
    let options = zune_core::options::DecoderOptions::default()
        .jpeg_set_out_colorspace(zune_core::colorspace::ColorSpace::RGBA);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(
        zune_core::bytestream::ZCursor::new(bytes),
        options,
    );
    decoder.decode_headers().map_err(|_| DecodeError::Corrupt)?;
    let info = decoder.info().ok_or(DecodeError::Corrupt)?;
    check_dims(u32::from(info.width), u32::from(info.height))?;

    let pixels = decoder.decode().map_err(|_| DecodeError::Corrupt)?;
    if pixels.len()
        != usize::try_from(info.width as u32 * info.height as u32).unwrap_or(usize::MAX) * 4
    {
        return Err(DecodeError::Corrupt);
    }
    Ok(Rgba8Image {
        width: u32::from(info.width),
        height: u32::from(info.height),
        pixels: pixels.into(),
    })
}

/// GIF's first frame through the `gif` crate, converted to RGBA. Frames that sit inside a larger
/// logical screen decode as themselves; compositing animation frames is future work (spec 3).
fn decode_gif(bytes: &[u8]) -> Result<Rgba8Image, DecodeError> {
    let mut options = gif::DecodeOptions::new();
    options.set_color_output(gif::ColorOutput::RGBA);
    let mut reader = options
        .read_info(std::io::Cursor::new(bytes))
        .map_err(|_| DecodeError::Corrupt)?;
    check_dims(u32::from(reader.width()), u32::from(reader.height()))?;

    // `next_frame_info` reads the frame header and its palette only: the pixel buffer is sized
    // and allocated by us, after the cap check on the frame's own dimensions.
    let frame = reader
        .next_frame_info()
        .map_err(|_| DecodeError::Corrupt)?
        .ok_or(DecodeError::Corrupt)?;
    let (width, height) = (u32::from(frame.width), u32::from(frame.height));
    check_dims(width, height)?;

    let size = reader.buffer_size();
    if size > MAX_IMAGE_BYTES as usize {
        return Err(DecodeError::TooLarge);
    }
    let mut buf = vec![0u8; size];
    reader
        .read_into_buffer(&mut buf)
        .map_err(|_| DecodeError::Corrupt)?;
    Ok(Rgba8Image {
        width,
        height,
        pixels: buf.into(),
    })
}

#[cfg(test)]
pub(crate) mod fixtures {
    use std::io::Cursor;

    /// The 4x4 test picture: a red, green, blue and white quadrant (top-left to bottom-right).
    /// Exact for the lossless formats; the JPEG fixture is encoded from the same colors without
    /// alpha.
    pub fn tiny_rgba() -> Vec<u8> {
        let quadrants = [(255u8, 0u8, 0u8), (0, 255, 0), (0, 0, 255), (255, 255, 255)];
        let mut pixels = Vec::with_capacity(4 * 4 * 4);
        for y in 0..4u32 {
            for x in 0..4u32 {
                let rgb = quadrants[usize::try_from((y / 2) * 2 + x / 2).unwrap()];
                pixels.extend_from_slice(&[rgb.0, rgb.1, rgb.2, 255]);
            }
        }
        pixels
    }

    /// The 8x8 picture the resample goldens are made from: a smooth gradient in every channel and
    /// an alpha that varies per pixel, so filters have something to disagree about.
    pub fn pattern_rgba() -> Vec<u8> {
        let mut pixels = Vec::with_capacity(8 * 8 * 4);
        for y in 0..8u8 {
            for x in 0..8u8 {
                pixels.extend_from_slice(&[
                    x * 32,
                    y * 32,
                    ((x + y) % 16) * 16,
                    255 - ((x + y) % 8) * 32,
                ]);
            }
        }
        pixels
    }

    fn encode_png(width: u32, height: u32, pixels: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(Cursor::new(&mut out), width, height);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc.write_header().unwrap();
            writer.write_image_data(pixels).unwrap();
            writer.finish().unwrap();
        }
        out
    }

    /// The JPEG fixture: the tiny picture's colors, baseline JPEG (RGB), quality 90. Encoded with
    /// `jpeg-encoder`, a dev-only dependency; the bytes are checked in like the PNGs.
    fn encode_jpeg() -> Vec<u8> {
        let rgb = tiny_rgba();
        let mut packed = Vec::with_capacity(4 * 4 * 3);
        for px in rgb.chunks(4) {
            packed.extend_from_slice(&px[..3]);
        }
        let mut out = Vec::new();
        jpeg_encoder::Encoder::new(Cursor::new(&mut out), 90)
            .encode(&packed, 4, 4, jpeg_encoder::ColorType::Rgb)
            .unwrap();
        out
    }

    /// The GIF fixture: the tiny picture's colors as an indexed 4x4 frame with a global palette.
    fn encode_gif() -> Vec<u8> {
        let palette: Vec<u8> = [(255u8, 0u8, 0u8), (0, 255, 0), (0, 0, 255), (255, 255, 255)]
            .iter()
            .flat_map(|(r, g, b)| [*r, *g, *b])
            .collect();
        // The same quadrant order as `tiny_rgba`, as palette indices: two of each color per row,
        // red/green on the top two rows, blue/white on the bottom two.
        let indexed = vec![0u8, 0, 1, 1, 0, 0, 1, 1, 2, 2, 3, 3, 2, 2, 3, 3];
        let mut out = Vec::new();
        {
            let mut enc = gif::Encoder::new(Cursor::new(&mut out), 4, 4, &palette).unwrap();
            enc.write_frame(&gif::Frame {
                width: 4,
                height: 4,
                buffer: std::borrow::Cow::Owned(indexed),
                ..gif::Frame::default()
            })
            .unwrap();
        }
        out
    }

    /// Rewrites every fixture under `tests/fixtures/images/` from these patterns. Run with
    /// `UPDATE_IMAGE_FIXTURES=1 cargo test -p fbterm-term` after changing a pattern or a codec;
    /// the files are checked in, so tests never depend on a platform's encoder.
    #[test]
    fn write_fixtures() {
        if std::env::var("UPDATE_IMAGE_FIXTURES").is_err() {
            return;
        }
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/images");
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(format!("{dir}/tiny.png"), encode_png(4, 4, &tiny_rgba())).unwrap();
        std::fs::write(
            format!("{dir}/pattern_8x8.png"),
            encode_png(8, 8, &pattern_rgba()),
        )
        .unwrap();
        std::fs::write(format!("{dir}/tiny.jpg"), encode_jpeg()).unwrap();
        std::fs::write(format!("{dir}/tiny.gif"), encode_gif()).unwrap();
    }

    pub const TINY_PNG: &[u8] = include_bytes!("../../tests/fixtures/images/tiny.png");
    pub const PATTERN_PNG: &[u8] = include_bytes!("../../tests/fixtures/images/pattern_8x8.png");
    pub const TINY_JPG: &[u8] = include_bytes!("../../tests/fixtures/images/tiny.jpg");
    pub const TINY_GIF: &[u8] = include_bytes!("../../tests/fixtures/images/tiny.gif");
}

#[cfg(test)]
mod tests {
    use super::fixtures::{PATTERN_PNG, TINY_GIF, TINY_JPG, TINY_PNG, tiny_rgba};
    use super::*;
    use feathercritty::event::ImageFormat;

    fn encoded(format: ImageFormat, bytes: &[u8]) -> ImageData {
        ImageData::Encoded {
            format,
            bytes: bytes.to_vec(),
            width: 4,
            height: 4,
        }
    }

    #[test]
    fn decodes_png_jpeg_gif() {
        let png = decode(encoded(ImageFormat::Png, TINY_PNG)).unwrap();
        assert_eq!((png.width, png.height), (4, 4));
        // PNG is lossless: the fixture's pixels come back exactly as written.
        assert_eq!(png.pixels, tiny_rgba());

        let gif = decode(encoded(ImageFormat::Gif, TINY_GIF)).unwrap();
        assert_eq!((gif.width, gif.height), (4, 4));
        // GIF is indexed: the fixture uses four exact palette entries, so the colors are exact.
        assert_eq!(gif.pixels, tiny_rgba());

        let jpeg = decode(encoded(ImageFormat::Jpeg, TINY_JPG)).unwrap();
        assert_eq!((jpeg.width, jpeg.height), (4, 4));
        // JPEG is lossy: every channel must stay near the color it was encoded from.
        let want = tiny_rgba();
        for (i, (got, want)) in jpeg.pixels.iter().zip(want.iter()).enumerate() {
            let diff = got.abs_diff(*want);
            assert!(diff <= 24, "pixel {i}: got {got}, want {want}");
        }

        // The gradient fixture decodes too (it is what the resample goldens start from).
        let pattern = decode(encoded(ImageFormat::Png, PATTERN_PNG)).unwrap();
        assert_eq!((pattern.width, pattern.height), (8, 8));
    }

    #[test]
    fn oversized_headers_are_refused_before_allocating() {
        // Patch the PNG's IHDR width to 20 000: over MAX_IMAGE_SIDE on a side. The IDAT still
        // describes a 4x4 image, so only a header-first check can refuse this before decoding.
        let mut wide = TINY_PNG.to_vec();
        wide[16..20].copy_from_slice(&20_000u32.to_be_bytes());
        assert_eq!(
            decode(encoded(ImageFormat::Png, &wide)),
            Err(DecodeError::TooLarge)
        );

        // 8192x8192: each side is under the cap, but one RGBA8 frame is 256 MiB, over 64.
        let mut huge = TINY_PNG.to_vec();
        huge[16..20].copy_from_slice(&8192u32.to_be_bytes());
        huge[20..24].copy_from_slice(&8192u32.to_be_bytes());
        assert_eq!(
            decode(encoded(ImageFormat::Png, &huge)),
            Err(DecodeError::TooLarge)
        );

        // Patch the GIF's logical screen width (little-endian, just after the 6-byte signature).
        let mut wide_gif = TINY_GIF.to_vec();
        wide_gif[6..8].copy_from_slice(&20_000u16.to_le_bytes());
        assert_eq!(
            decode(encoded(ImageFormat::Gif, &wide_gif)),
            Err(DecodeError::TooLarge)
        );
    }

    #[test]
    fn giant_iccp_profile_answers_toolarge() {
        // The png crate reports `LimitsExceeded` when its own default 64 MiB allocation budget
        // runs out. IDAT streams are inflated as they arrive and bypass that accounting, but any
        // other chunk must be buffered whole, and the buffering loop refuses to grow past the
        // budget — a chunk declared larger than it is what trips it, deterministically, inside
        // `read_info`. An iCCP profile is the natural such chunk: ours never needs the profile,
        // so this input is perfectly well-formed and merely enormous — `TooLarge`, not `Corrupt`.
        //
        // The fixture: TINY_PNG's real signature and IHDR, then an iCCP chunk of ~67 MiB whose
        // profile is a zlib stream of 1025 stored deflate blocks (valid all the way through; it
        // would inflate to just over 64 MiB). The budget trips mid-chunk, before its CRC is even
        // read — the checksum and profile are here so the fixture stays an honest PNG.
        let file = giant_iccp_fixture();

        assert_eq!(
            decode(encoded(ImageFormat::Png, &file)),
            Err(DecodeError::TooLarge),
            "a 67 MiB iCCP chunk on a 4x4 frame must be refused as too large",
        );
    }

    /// Builds the giant-iCCP fixture (see [`giant_iccp_profile_answers_toolarge`]).
    fn giant_iccp_fixture() -> Vec<u8> {
        let blocks = 1025usize; // 1025 * 65535 inflates just past the crate's 64 MiB budget
        let mut file = TINY_PNG.to_vec();
        // Walk to IDAT chunk by chunk: prefix keeps everything before it, tail is the original
        // (small) IDAT and IEND, which the parse never reaches.
        let mut at = 8; // past the signature
        loop {
            let len = u32::from_be_bytes(file[at..at + 4].try_into().unwrap()) as usize;
            if &file[at + 4..at + 8] == b"IDAT" {
                break;
            }
            at += 12 + len; // length, type, data, CRC
        }
        let tail = file[at..].to_vec();
        file.truncate(at);

        let block_len = 5 + 65535; // header (BFINAL=0, stored, LEN, NLEN) plus its payload run
        let data_len = 2 + 1 + 2 + blocks * block_len; // "c\0" keyword, method byte, zlib header
        file.extend((4 + data_len as u32).to_be_bytes()); // chunk length covers type too
        let crc_start = file.len();
        file.extend_from_slice(b"iCCP");
        file.extend_from_slice(b"c\x00"); // profile name (null-terminated) and…
        file.push(0x00); // …its compression method: deflate
        file.extend_from_slice(&[0x78, 0x9c]); // zlib header: deflate, 32K window, no dictionary
        let run_start = file.len();
        file.extend_from_slice(&[0x00, 0xff, 0xff, 0x00, 0x00]); // first stored block's header
        file.resize(run_start + block_len, 0x00); // its 65535 zero bytes of payload
        let mut run = block_len;
        while run < blocks * block_len {
            file.extend_from_within(run_start..run_start + run); // doubling keeps block alignment
            run *= 2;
        }
        file.truncate(run_start + blocks * block_len);
        let crc = crc32(&file[crc_start..]);
        file.extend(crc.to_be_bytes());
        file.extend_from_slice(&tail);
        file
    }

    /// CRC-32 (the PNG variant) over a byte slice — the fixture's chunks need real checksums.
    fn crc32(bytes: &[u8]) -> u32 {
        let mut table = [0u32; 256];
        for (i, slot) in table.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                *slot = if c & 1 != 0 {
                    0xedb8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
                // Reusing `slot` as the running value across the eight rounds.
                c = *slot;
            }
        }
        let mut crc = 0xffff_ffffu32;
        for &b in bytes {
            crc = table[((crc ^ u32::from(b)) & 0xff) as usize] ^ (crc >> 8);
        }
        !crc
    }

    #[test]
    fn corrupt_and_panicking_decodes_are_contained() {
        for format in [ImageFormat::Png, ImageFormat::Jpeg, ImageFormat::Gif] {
            assert_eq!(
                decode(encoded(format, b"not an image at all")),
                Err(DecodeError::Corrupt),
                "{format:?} of junk"
            );
            assert_eq!(
                decode(encoded(format, &[])),
                Err(DecodeError::Corrupt),
                "{format:?} empty"
            );
        }
        // A PNG cut short behind its header: readable header, unusable data.
        let mut truncated = TINY_PNG.to_vec();
        truncated.truncate(24);
        assert_eq!(
            decode(encoded(ImageFormat::Png, &truncated)),
            Err(DecodeError::Corrupt)
        );

        // A decoder that panics (injected through the test hook) is Corrupt, not a crash...
        let boom: DecodeHook = Arc::new(|data| {
            if matches!(data, ImageData::Encoded { .. }) {
                panic!("test: the decoder panicked")
            }
            None
        });
        assert_eq!(
            decode_with(&encoded(ImageFormat::Png, TINY_PNG), Some(&boom)),
            Err(DecodeError::Corrupt)
        );
        // ...the hook can also answer for the decoders (used by the store's tests)...
        let refuse: DecodeHook = Arc::new(|_| Some(Err(DecodeError::TooLarge)));
        assert_eq!(
            decode_with(&encoded(ImageFormat::Png, TINY_PNG), Some(&refuse)),
            Err(DecodeError::TooLarge)
        );
        // ...and answering None leaves the real decoders alone.
        let pass: DecodeHook = Arc::new(|_| None);
        assert!(decode_with(&encoded(ImageFormat::Png, TINY_PNG), Some(&pass)).is_ok());

        // The raw pixels path never panics and needs no container.
        let rgba = ImageData::Rgba {
            width: 1,
            height: 1,
            pixels: vec![1, 2, 3, 4],
        };
        assert_eq!(decode(rgba).unwrap().pixels.to_vec(), vec![1, 2, 3, 4]);
    }
}
