//! Decoding, metadata stripping and resizing (CPU work: call it through
//! `engine_core::background::run`).

use std::io::Cursor;

use image::codecs::jpeg::JpegEncoder;
use image::codecs::webp::WebPEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits};

use crate::MediaConfig;
use crate::sniff::Kind;

/// A file ready to store.
#[derive(Debug)]
pub struct Processed {
    pub kind: Kind,
    /// The bytes to keep as "the original" (re-encoded for JPEG, PNG and
    /// WebP: no metadata left).
    pub original: Vec<u8>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Resized copies, smallest first.
    pub variants: Vec<(u32, Vec<u8>)>,
}

/// Why processing failed (shown to the uploader).
#[derive(Debug, thiserror::Error)]
pub enum ProcessError {
    #[error("the image is {w}×{h} pixels; the limit is {max} million pixels")]
    TooManyPixels { w: u32, h: u32, max: u64 },
    #[error("the file could not be read as {0}: {1}")]
    Broken(&'static str, String),
}

fn format_of(kind: Kind) -> Option<ImageFormat> {
    match kind {
        Kind::Jpeg => Some(ImageFormat::Jpeg),
        Kind::Png => Some(ImageFormat::Png),
        Kind::Gif => Some(ImageFormat::Gif),
        Kind::Webp => Some(ImageFormat::WebP),
        Kind::Pdf => None,
    }
}

fn encode(img: &DynamicImage, kind: Kind, quality: u8) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    match kind {
        Kind::Jpeg => {
            // JPEG has no alpha channel.
            let rgb = img.to_rgb8();
            JpegEncoder::new_with_quality(&mut out, quality)
                .encode_image(&rgb)
                .map_err(|e| e.to_string())?;
        }
        Kind::Webp => {
            // The `image` crate writes lossless WebP.
            let rgba = img.to_rgba8();
            WebPEncoder::new_lossless(&mut out)
                .encode(
                    rgba.as_raw(),
                    rgba.width(),
                    rgba.height(),
                    image::ExtendedColorType::Rgba8,
                )
                .map_err(|e| e.to_string())?;
        }
        // GIF variants and PNG are written as PNG (lossless).
        Kind::Png | Kind::Gif => {
            img.write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
                .map_err(|e| e.to_string())?;
        }
        Kind::Pdf => unreachable!("PDFs are not images"),
    }
    Ok(out)
}

/// The file type of a variant of `kind` (GIF variants are PNG).
pub fn variant_kind(kind: Kind) -> Kind {
    if kind == Kind::Gif { Kind::Png } else { kind }
}

/// Validate and prepare an upload whose type was sniffed as `kind`.
pub fn process(bytes: &[u8], kind: Kind, cfg: &MediaConfig) -> Result<Processed, ProcessError> {
    let Some(format) = format_of(kind) else {
        return Ok(Processed {
            kind,
            original: bytes.to_vec(),
            width: None,
            height: None,
            variants: Vec::new(),
        });
    };
    let name = kind.mime();
    let max_pixels = cfg.max_megapixels * 1_000_000;
    // Read the size from the header first, so a decompression bomb is
    // refused before any pixel memory is allocated.
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = Limits::default();
    limits.max_image_width = Some(30_000);
    limits.max_image_height = Some(30_000);
    limits.max_alloc = Some(max_pixels.saturating_mul(8).max(64 * 1024 * 1024));
    reader.limits(limits);
    let mut decoder = reader
        .into_decoder()
        .map_err(|e| ProcessError::Broken(name, e.to_string()))?;
    let (w, h) = decoder.dimensions();
    if u64::from(w) * u64::from(h) > max_pixels {
        return Err(ProcessError::TooManyPixels {
            w,
            h,
            max: cfg.max_megapixels,
        });
    }
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = DynamicImage::from_decoder(decoder)
        .map_err(|e| ProcessError::Broken(name, e.to_string()))?;
    img.apply_orientation(orientation);
    let (w, h) = (img.width(), img.height());
    let original = if kind == Kind::Gif {
        // Keep GIFs as uploaded (animation); GIF has no EXIF block.
        bytes.to_vec()
    } else {
        encode(&img, kind, cfg.jpeg_quality).map_err(|e| ProcessError::Broken(name, e))?
    };
    let mut widths: Vec<u32> = cfg.widths.iter().copied().filter(|x| *x < w).collect();
    widths.sort_unstable();
    widths.dedup();
    let vk = variant_kind(kind);
    let mut variants = Vec::new();
    for vw in widths {
        let vh = ((u64::from(h) * u64::from(vw)) / u64::from(w)).max(1) as u32;
        let small = img.resize_exact(vw, vh, FilterType::Lanczos3);
        let data =
            encode(&small, vk, cfg.jpeg_quality).map_err(|e| ProcessError::Broken(name, e))?;
        variants.push((vw, data));
    }
    Ok(Processed {
        kind,
        original,
        width: Some(w),
        height: Some(h),
        variants,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A JPEG with an EXIF block carrying a fake GPS marker.
    pub fn jpeg_with_exif(w: u32, h: u32) -> Vec<u8> {
        let img = DynamicImage::new_rgb8(w, h);
        let mut plain = Vec::new();
        JpegEncoder::new_with_quality(&mut plain, 90)
            .encode_image(&img.to_rgb8())
            .unwrap();
        // Insert an APP1 "Exif" segment after SOI.
        let payload = b"Exif\0\0GPS-SECRET-LOCATION";
        let len = (payload.len() + 2) as u16;
        let mut out = vec![0xFF, 0xD8, 0xFF, 0xE1];
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(payload);
        out.extend_from_slice(&plain[2..]);
        out
    }

    #[test]
    fn jpeg_metadata_is_dropped_and_variants_made() {
        let cfg = MediaConfig::default();
        let bytes = jpeg_with_exif(1000, 500);
        assert!(bytes.windows(10).any(|w| w == b"GPS-SECRET"));
        let p = process(&bytes, Kind::Jpeg, &cfg).unwrap();
        assert_eq!((p.width, p.height), (Some(1000), Some(500)));
        assert!(!p.original.windows(10).any(|w| w == b"GPS-SECRET"));
        let widths: Vec<u32> = p.variants.iter().map(|v| v.0).collect();
        assert_eq!(widths, [320, 640, 960]);
        let small = image::load_from_memory(&p.variants[0].1).unwrap();
        assert_eq!((small.width(), small.height()), (320, 160));
    }

    #[test]
    fn pixel_bombs_are_refused_before_decoding() {
        let cfg = MediaConfig {
            max_megapixels: 1,
            ..Default::default()
        };
        let img = DynamicImage::new_rgb8(2000, 1000);
        let mut png = Vec::new();
        img.write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
            .unwrap();
        let err = process(&png, Kind::Png, &cfg).unwrap_err();
        assert!(matches!(err, ProcessError::TooManyPixels { .. }), "{err}");
    }

    #[test]
    fn broken_images_are_refused() {
        let cfg = MediaConfig::default();
        let err = process(b"\x89PNG\r\n\x1a\ngarbage", Kind::Png, &cfg).unwrap_err();
        assert!(matches!(err, ProcessError::Broken(..)), "{err}");
    }
}
