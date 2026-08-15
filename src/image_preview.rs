//! Bounded, read-only raster previews for the review pane.
//!
//! The module accepts only already-authorized bytes.  It never opens paths, emits terminal
//! escapes, or invokes a helper: `ui` paints its reduced RGBA result as ordinary truecolor
//! Unicode halfblocks.  This keeps Files-only access at its descriptor capability boundary.

use std::io::Cursor;

use image::{DynamicImage, ImageReader, Limits, RgbaImage, imageops::FilterType};

/// Largest accepted encoded source.  This is independent of the text-diff budget because image
/// headers compress pixels aggressively; dimensions/pixels are bounded separately below.
pub const MAX_SOURCE_BYTES: usize = 2 * 1024 * 1024;
/// No source dimension may exceed this many pixels.
pub const MAX_SOURCE_SIDE: u32 = 8_192;
/// Maximum source pixel count accepted before raster allocation.
pub const MAX_SOURCE_PIXELS: u64 = 16_000_000;
/// Cached preview raster cap.  Halfblocks consume two source pixels per terminal cell.
pub const MAX_PREVIEW_WIDTH: u32 = 160;
pub const MAX_PREVIEW_HEIGHT: u32 = 96;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageKind {
    Png,
    Jpeg,
    Webp,
    Gif,
    Svg,
}

impl ImageKind {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Png => "PNG",
            Self::Jpeg => "JPEG",
            Self::Webp => "WebP",
            Self::Gif => "GIF · first frame",
            Self::Svg => "SVG",
        }
    }
}

/// A decoded, downscaled still image.  The original source bytes never remain in this value.
#[derive(Clone, Debug)]
pub struct ImagePreview {
    pub kind: ImageKind,
    pub source_width: u32,
    pub source_height: u32,
    pub pixels: RgbaImage,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImagePreviewError {
    NotImage,
    SvgUnavailable,
    TooLarge,
    Malformed,
}

/// Strictly sniff a supported format.  Extension names are deliberately not trusted.
#[must_use]
pub fn sniff(bytes: &[u8]) -> Option<ImageKind> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(ImageKind::Png)
    } else if bytes.len() >= 3 && bytes[..3] == [0xff, 0xd8, 0xff] {
        Some(ImageKind::Jpeg)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(ImageKind::Gif)
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some(ImageKind::Webp)
    } else if looks_like_svg(bytes) {
        Some(ImageKind::Svg)
    } else {
        None
    }
}

const SVG_PROLOGUE_LIMIT: usize = 8 * 1024;

/// Recognize an SVG root through a deliberately tiny, bounded lexical prologue scanner.
///
/// This is not XML parsing and never renders SVG. It skips only a UTF-8 BOM, ASCII whitespace,
/// an XML declaration, comments, and a doctype. The first remaining construct must be a real
/// `<svg>` start tag, so textual/CDATA/comment occurrences and `<svgx>` are not images.
fn looks_like_svg(bytes: &[u8]) -> bool {
    let prefix = &bytes[..bytes.len().min(SVG_PROLOGUE_LIMIT)];
    if std::str::from_utf8(prefix).is_err() {
        return false;
    }
    let mut at = 0;
    if prefix.starts_with(&[0xef, 0xbb, 0xbf]) {
        at = 3;
    }

    loop {
        at = skip_ascii_whitespace(prefix, at);
        let rest = &prefix[at..];
        if rest.starts_with(b"<?xml") {
            let Some(next) = rest.get(5) else { return false };
            if !next.is_ascii_whitespace() {
                return false;
            }
            let Some(end) = rest.windows(2).position(|window| window == b"?>") else {
                return false;
            };
            at += end + 2;
        } else if rest.starts_with(b"<!--") {
            let Some(end) = rest.windows(3).position(|window| window == b"-->") else {
                return false;
            };
            at += end + 3;
        } else if rest.starts_with(b"<!DOCTYPE") {
            let Some(next) = rest.get(9) else { return false };
            if !next.is_ascii_whitespace() {
                return false;
            }
            let Some(end) = doctype_end(rest) else { return false };
            at += end + 1;
        } else {
            return rest.strip_prefix(b"<svg").is_some_and(|tail| {
                tail.first()
                    .is_some_and(|byte| byte.is_ascii_whitespace() || matches!(byte, b'/' | b'>'))
            });
        }
        if at >= prefix.len() {
            return false;
        }
    }
}

fn skip_ascii_whitespace(bytes: &[u8], mut at: usize) -> usize {
    while bytes.get(at).is_some_and(u8::is_ascii_whitespace) {
        at += 1;
    }
    at
}

/// Return the final `>` of one bounded doctype, tolerating quoted literals and an internal
/// subset. An unclosed quote/subset/doctype is rejected rather than scanning past the prologue.
fn doctype_end(bytes: &[u8]) -> Option<usize> {
    let mut quote = None;
    let mut subset_depth = 0_u32;
    for (i, &byte) in bytes.iter().enumerate().skip(9) {
        if let Some(open) = quote {
            if byte == open {
                quote = None;
            }
            continue;
        }
        match byte {
            b'\'' | b'\"' => quote = Some(byte),
            b'[' => subset_depth = subset_depth.checked_add(1)?,
            b']' if subset_depth > 0 => subset_depth -= 1,
            b'>' if subset_depth == 0 => return Some(i),
            _ => {}
        }
    }
    None
}

/// Decode one bounded still frame.  `image`'s GIF decoder exposes the first frame through the
/// normal `decode` path; no animation frames are collected or scheduled.
pub fn decode(bytes: &[u8]) -> Result<ImagePreview, ImagePreviewError> {
    if bytes.len() > MAX_SOURCE_BYTES {
        return Err(ImagePreviewError::TooLarge);
    }
    let kind = sniff(bytes).ok_or(ImagePreviewError::NotImage)?;
    if kind == ImageKind::Svg {
        return Err(ImagePreviewError::SvgUnavailable);
    }
    let format = match kind {
        ImageKind::Png => image::ImageFormat::Png,
        ImageKind::Jpeg => image::ImageFormat::Jpeg,
        ImageKind::Webp => image::ImageFormat::WebP,
        ImageKind::Gif => image::ImageFormat::Gif,
        ImageKind::Svg => unreachable!(),
    };
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SOURCE_SIDE);
    limits.max_image_height = Some(MAX_SOURCE_SIDE);
    limits.max_alloc = Some(MAX_SOURCE_PIXELS * 4);
    reader.limits(limits);
    let (source_width, source_height) =
        reader.into_dimensions().map_err(|_| ImagePreviewError::Malformed)?;
    if source_width == 0
        || source_height == 0
        || u64::from(source_width) * u64::from(source_height) > MAX_SOURCE_PIXELS
    {
        return Err(ImagePreviewError::TooLarge);
    }
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SOURCE_SIDE);
    limits.max_image_height = Some(MAX_SOURCE_SIDE);
    limits.max_alloc = Some(MAX_SOURCE_PIXELS * 4);
    reader.limits(limits);
    let decoded = reader.decode().map_err(|_| ImagePreviewError::Malformed)?;
    let pixels = fit(decoded, MAX_PREVIEW_WIDTH, MAX_PREVIEW_HEIGHT);
    Ok(ImagePreview { kind, source_width, source_height, pixels })
}

fn fit(image: DynamicImage, max_width: u32, max_height: u32) -> RgbaImage {
    let (width, height) = (image.width(), image.height());
    if width <= max_width && height <= max_height {
        return image.into_rgba8();
    }
    image.resize(max_width, max_height, FilterType::Triangle).into_rgba8()
}

#[cfg(test)]
mod tests {
    use super::{ImageKind, ImagePreviewError, MAX_SOURCE_BYTES, decode, sniff};
    use image::{ImageFormat, Rgba, RgbaImage};

    fn png() -> Vec<u8> {
        let image = RgbaImage::from_pixel(2, 2, Rgba([255, 0, 0, 255]));
        let mut bytes = Vec::new();
        image.write_to(&mut std::io::Cursor::new(&mut bytes), ImageFormat::Png).unwrap();
        bytes
    }

    #[test]
    fn sniff_and_decode_png_without_paths() {
        let bytes = png();
        assert_eq!(sniff(&bytes), Some(ImageKind::Png));
        let preview = decode(&bytes).unwrap();
        assert_eq!((preview.source_width, preview.source_height), (2, 2));
        assert_eq!(preview.pixels.dimensions(), (2, 2));
    }

    #[test]
    fn svg_is_recognized_but_never_parsed_in_v1() {
        assert_eq!(sniff(br#"<svg width=\"1\" height=\"1\"/>"#), Some(ImageKind::Svg));
        assert!(matches!(decode(br"<svg/>"), Err(ImagePreviewError::SvgUnavailable)));
    }

    #[test]
    fn svg_xml_prologue_with_comment_and_doctype_is_stably_unavailable() {
        let svg = br#"<?xml version="1.0"?>
<!-- generated -->
<!DOCTYPE svg>
<svg/>"#;
        assert_eq!(sniff(svg), Some(ImageKind::Svg));
        assert!(matches!(decode(svg), Err(ImagePreviewError::SvgUnavailable)));
    }

    #[test]
    fn svg_scanner_accepts_only_a_real_root_after_legal_prologue() {
        for svg in [
            b"\xef\xbb\xbf \n<?xml version=\"1.0\"?>\n<!-- comment -->\n<!DOCTYPE svg [<!ELEMENT svg ANY>]>\n<svg viewBox=\"0 0 1 1\">".as_slice(),
            b"\n<!-- c -->\n<svg/>".as_slice(),
            b"<!DOCTYPE svg SYSTEM \"about:blank\"><svg/>".as_slice(),
        ] {
            assert_eq!(sniff(svg), Some(ImageKind::Svg), "{svg:?}");
        }
    }

    #[test]
    fn svg_scanner_rejects_tokens_that_are_not_a_root_element() {
        for not_svg in [
            b"<svgx/>".as_slice(),
            b"<!-- <svg/> -->".as_slice(),
            b"<![CDATA[<svg/>]]>".as_slice(),
            b"text <svg/>".as_slice(),
            b"<html><svg/></html>".as_slice(),
            b"<?xml version=\"1.0\"".as_slice(),
            b"<!-- unterminated <svg/>".as_slice(),
            b"<!DOCTYPE svg [<x>]".as_slice(),
        ] {
            assert_eq!(sniff(not_svg), None, "{not_svg:?}");
        }
    }

    #[test]
    fn rejects_unknown_and_over_cap_inputs() {
        assert!(matches!(decode(b"not an image"), Err(ImagePreviewError::NotImage)));
        let bytes = vec![0; MAX_SOURCE_BYTES + 1];
        assert!(matches!(decode(&bytes), Err(ImagePreviewError::TooLarge)));
    }
}
