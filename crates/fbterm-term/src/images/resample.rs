//! Resampling decoded images to the pixel box of a placement.
//!
//! A placement keeps its cell span; when the font, line height or display scale changes, the
//! image is scaled to fit inside that span with its aspect ratio kept, anchored top-left and never
//! stretched (spec goal 3). [`fit_size`] computes the fitted pixel size from the source and the
//! box; [`resample`] produces exactly that size. Per the plan's scaling constraint: nearest
//! neighbour when the scale factor is an integer and the source is at most [`NEAREST_MAX_SIDE`]
//! pixels on a side (pixel art stays crisp), Lanczos3 otherwise.
//!
//! Tests run the resizer on its plain scalar code path (`CpuExtensions::None`): SIMD kernels round
//! differently across CPU families, and pinning one path keeps the golden fixtures below comparable
//! byte-for-byte (nearest) and within their tolerance (Lanczos) on every machine. Production builds
//! skip the pin and use whatever the CPU offers — a user's resample is judged by eye, not against a
//! fixture.

use fast_image_resize as fr;

use crate::images::decode::Rgba8Image;

/// The largest source (per side) that nearest neighbour may be chosen for, so only small pixel
/// art is ever hard-scaled (plan scaling constraint).
pub const NEAREST_MAX_SIDE: u32 = 256;

/// The pixel size an image of `src` pixels fitted inside a `box` box lands on: the largest size
/// that fits with its aspect ratio kept, rounding to whole pixels, at least 1x1. Upscaling fills
/// the box in the binding dimension; nothing is stretched.
pub fn fit_size(src_w: u32, src_h: u32, box_w: u32, box_h: u32) -> (u32, u32) {
    let scale = (f64::from(box_w) / f64::from(src_w)).min(f64::from(box_h) / f64::from(src_h));
    let w = ((f64::from(src_w) * scale).round() as u32).clamp(1, box_w);
    let h = ((f64::from(src_h) * scale).round() as u32).clamp(1, box_h);
    (w, h)
}

/// Whether resizing from `src` to `out` is an exact integer multiple of the source size (1 counts;
/// a shrink never does). With this true and a small enough source, [`resample`] uses nearest.
pub fn is_integer_multiple(src_w: u32, src_h: u32, out_w: u32, out_h: u32) -> bool {
    src_w > 0
        && src_h > 0
        && out_w.checked_mul(src_h) == out_h.checked_mul(src_w)
        && out_w >= src_w
        && out_w.is_multiple_of(src_w)
}

/// Which filter the rule picks for one resize (exposed so the rule itself is testable).
fn pick_filter(src: &Rgba8Image, out_w: u32, out_h: u32, integer_scale: bool) -> fr::ResizeAlg {
    if integer_scale
        && is_integer_multiple(src.width, src.height, out_w, out_h)
        && src.width <= NEAREST_MAX_SIDE
        && src.height <= NEAREST_MAX_SIDE
    {
        fr::ResizeAlg::Nearest
    } else {
        fr::ResizeAlg::Convolution(fr::FilterType::Lanczos3)
    }
}

/// Resample `src` to exactly `target_px` pixels.
///
/// `integer_scale` tells the filter rule the placement's geometry is an exact integer zoom of the
/// source; combined with a small source it selects nearest neighbour, otherwise Lanczos3. The
/// alpha channel is weighted during filtering (fast_image_resize's premultiply-aware path).
pub fn resample(src: &Rgba8Image, target_px: (u32, u32), integer_scale: bool) -> Rgba8Image {
    let (tw, th) = target_px;
    if tw == 0 || th == 0 {
        return nearest(src, 1, 1);
    }
    if tw == src.width && th == src.height {
        // Identity: the filter would only re-derive the same pixels. The clone shares the buffer —
        // `Bytes` is reference counted — so even here no frame is copied (finding P4).
        return src.clone();
    }
    let options = fr::ResizeOptions::new().resize_alg(pick_filter(src, tw, th, integer_scale));
    let resized: Result<Vec<u8>, String> = (|| {
        let src_img =
            fr::images::ImageRef::new(src.width, src.height, &src.pixels, fr::PixelType::U8x4)
                .map_err(|e| e.to_string())?;
        let mut dst = fr::images::Image::new(tw, th, fr::PixelType::U8x4);
        let mut resizer = fr::Resizer::new();
        // SAFETY: `None` selects the plain scalar path, which needs no CPU feature at all; the call
        // itself only stores the choice. Tests pin it so the golden fixtures below are comparable
        // byte-for-byte on every CPU family; production takes whatever the machine has, because a
        // user's resample is judged by eye, not against a fixture.
        #[cfg(test)]
        unsafe {
            resizer.set_cpu_extensions(fr::CpuExtensions::None)
        };
        resizer
            .resize(&src_img, &mut dst, Some(&options))
            .map_err(|e| e.to_string())?;
        Ok(dst.buffer().to_vec())
    })();
    match resized {
        Ok(pixels) => Rgba8Image {
            width: tw,
            height: th,
            pixels: pixels.into(),
        },
        Err(e) => {
            // Sizes are capped by the store long before this; an error here is a decoder giving a
            // buffer that does not match its header. Fall back to nearest rather than lose it.
            tracing::warn!("resampling failed ({e}); using nearest neighbour");
            nearest(src, tw, th)
        }
    }
}

/// Plain nearest-neighbour resize: the fallback when the library declines a payload.
fn nearest(src: &Rgba8Image, tw: u32, th: u32) -> Rgba8Image {
    let mut pixels = Vec::with_capacity((tw as usize) * (th as usize) * 4);
    for y in 0..th {
        let sy = ((u64::from(y) * u64::from(src.height)) / u64::from(th.max(1)))
            .min(u64::from(src.height - 1)) as u32;
        for x in 0..tw {
            let sx = ((u64::from(x) * u64::from(src.width)) / u64::from(tw.max(1)))
                .min(u64::from(src.width - 1)) as u32;
            let i = ((sy as usize) * (src.width as usize) + sx as usize) * 4;
            pixels.extend_from_slice(&src.pixels[i..i + 4]);
        }
    }
    Rgba8Image {
        width: tw,
        height: th,
        pixels: pixels.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::images::decode::{decode, fixtures::PATTERN_PNG};
    use feathercritty::event::{ImageData, ImageFormat};

    const TOLERANCE: u8 = 1;

    fn pattern() -> Rgba8Image {
        decode(ImageData::Encoded {
            format: ImageFormat::Png,
            bytes: PATTERN_PNG.to_vec(),
            width: 8,
            height: 8,
        })
        .unwrap()
    }

    fn ref_path(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/images")
            .join(name)
    }

    /// The golden buffers for scales 1, 1.25, 1.5 and 2 of the 8x8 pattern: nearest results are
    /// compared byte-exact, Lanczos results within 1 per channel (platform libm may differ in the
    /// last bit of a filter weight). `UPDATE_IMAGE_FIXTURES=1` rewrites them.
    #[test]
    fn golden_resample_at_scales() {
        let src = pattern();
        let goldens: [(&str, u32, bool); 4] = [
            ("pattern_8x8_to_8x8_nearest.rgba", 8, true),
            ("pattern_8x8_to_10x10_lanczos.rgba", 10, false),
            ("pattern_8x8_to_12x12_lanczos.rgba", 12, false),
            ("pattern_8x8_to_16x16_nearest.rgba", 16, true),
        ];
        for (name, side, integer) in goldens {
            let got = resample(&src, (side, side), integer);
            assert_eq!((got.width, got.height), (side, side), "{name}");
            if std::env::var("UPDATE_IMAGE_FIXTURES").is_ok() {
                std::fs::write(ref_path(name), &got.pixels).unwrap();
            }
            let want = std::fs::read(ref_path(name)).unwrap();
            assert_eq!(want.len(), got.pixels.len(), "{name} length");
            if integer {
                assert_eq!(want, got.pixels, "{name} nearest must be exact");
            } else {
                for (i, (g, w)) in got.pixels.iter().zip(want.iter()).enumerate() {
                    let diff = g.abs_diff(*w);
                    assert!(diff <= TOLERANCE, "{name} byte {i}: got {g}, want {w}");
                }
            }
        }
    }

    #[test]
    fn fit_keeps_aspect_inside_the_box() {
        // Square source: the box sizes are exact scales.
        assert_eq!(fit_size(8, 8, 10, 10), (10, 10));
        // Wide into tall: height binds, width keeps the ratio.
        assert_eq!(fit_size(8, 4, 10, 10), (10, 5));
        // Tall into wide: width binds.
        assert_eq!(fit_size(3, 12, 6, 6), (2, 6));
        // Shrinking keeps aspect too, and never rounds to zero.
        assert_eq!(fit_size(10, 10, 3, 3), (3, 3));
        assert_eq!(fit_size(100, 3, 1, 40), (1, 1));
    }

    #[test]
    fn filter_rule_nearest_only_for_small_integer_scales() {
        let src = Rgba8Image {
            width: 8,
            height: 8,
            pixels: vec![0u8; 8 * 8 * 4].into(),
        };
        // Integer zooms of a small source use nearest...
        assert_eq!(pick_filter(&src, 16, 16, true), fr::ResizeAlg::Nearest);
        assert_eq!(pick_filter(&src, 8, 8, true), fr::ResizeAlg::Nearest);
        // ...only when the caller says the geometry is an integer scale...
        assert_ne!(pick_filter(&src, 16, 16, false), fr::ResizeAlg::Nearest);
        // ...only for exact multiples (0.5x is not an integer scale)...
        assert!(!is_integer_multiple(8, 8, 4, 4));
        assert!(is_integer_multiple(8, 8, 16, 16));
        assert!(is_integer_multiple(8, 8, 8, 8));
        // ...and only for sources at most 256 px on a side.
        let big = Rgba8Image {
            width: 300,
            height: 300,
            pixels: vec![0u8; 300 * 300 * 4].into(),
        };
        assert_ne!(
            pick_filter(&big, 600, 600, true),
            fr::ResizeAlg::Nearest,
            "a large source must use Lanczos even at an integer zoom"
        );
    }

    #[test]
    fn resample_produces_the_requested_size_deterministically() {
        let src = pattern();
        let a = resample(&src, (13, 13), false);
        let b = resample(&src, (13, 13), false);
        assert_eq!(a.pixels, b.pixels, "resampling twice must agree");
        assert_eq!((a.width, a.height), (13, 13));
        // The identity resize returns the source untouched.
        assert_eq!(resample(&src, (8, 8), true).pixels, src.pixels);
    }
}
