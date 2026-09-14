//! A photograph, and the thumbnail of a video message.
//!
//! The clients send a camera shot as a JPEG at quality 80, no longer than
//! 1280 pixels on its long side — a size that survives a phone screen and
//! costs a fraction of what the sensor produced. The same encoder writes a
//! video message's 320-pixel poster, which is the frame a row draws before
//! anything is downloaded.
//!
//! Pure Rust, because this runs wherever a capture does: the window's
//! thread on a Mac, a recorder's thread on a phone, and a test with no
//! camera at all. The scaling is a box average rather than a filter — a
//! shot is being made smaller, and an average of the pixels it covers is
//! both what a box filter is and what the eye expects.

/// How long a photograph's long side may be. The clients' cap.
pub const PHOTO_MAX: usize = 1280;

/// A video message's poster, square.
pub const THUMBNAIL: usize = 320;

/// What a shot is encoded at: the clients' quality.
const QUALITY: u8 = 80;

/// Tightly packed RGB as a JPEG, scaled down so that neither side is longer
/// than `max_side`.
///
/// # Errors
///
/// If the dimensions do not match the bytes, are zero, or are past what a
/// JPEG can address.
pub fn of_rgb(rgb: &[u8], width: usize, height: usize, max_side: usize) -> Result<Vec<u8>, String> {
    if width == 0 || height == 0 {
        return Err("a picture with no size".to_string());
    }
    if rgb.len() < width * height * 3 {
        return Err(format!(
            "{} bytes is not a {width}×{height} picture",
            rgb.len()
        ));
    }
    let (w, h) = fitted(width, height, max_side);
    let scaled = (w != width || h != height).then(|| box_down(rgb, width, height, w, h));
    let (w16, h16) = (
        u16::try_from(w).map_err(|_| format!("{w} pixels is wider than a JPEG"))?,
        u16::try_from(h).map_err(|_| format!("{h} pixels is taller than a JPEG"))?,
    );
    let pixels = scaled.as_deref().unwrap_or(rgb);
    let mut out = Vec::new();
    jpeg_encoder::Encoder::new(&mut out, QUALITY)
        .encode(&pixels[..w * h * 3], w16, h16, jpeg_encoder::ColorType::Rgb)
        .map_err(|e| format!("jpeg: {e}"))?;
    Ok(out)
}

/// The size a picture is encoded at: itself, or its shape with the long side
/// brought down to `max_side`.
///
/// Public because a caller wants the size it will get — a photograph's row
/// says it, and asking for it twice must give the same answer as the
/// encoder's own arithmetic.
#[must_use]
pub fn fitted(width: usize, height: usize, max_side: usize) -> (usize, usize) {
    let long = width.max(height);
    if max_side == 0 || long <= max_side || long == 0 {
        return (width, height);
    }
    (
        (width * max_side / long).max(1),
        (height * max_side / long).max(1),
    )
}

/// Every output pixel is the average of the input pixels it covers.
fn box_down(rgb: &[u8], width: usize, height: usize, w: usize, h: usize) -> Vec<u8> {
    let mut out = vec![0u8; w * h * 3];
    for y in 0..h {
        let y0 = y * height / h;
        let y1 = (((y + 1) * height).div_ceil(h)).max(y0 + 1).min(height);
        for x in 0..w {
            let x0 = x * width / w;
            let x1 = (((x + 1) * width).div_ceil(w)).max(x0 + 1).min(width);
            let (mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32);
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let i = (sy * width + sx) * 3;
                    r += u32::from(rgb[i]);
                    g += u32::from(rgb[i + 1]);
                    b += u32::from(rgb[i + 2]);
                    n += 1;
                }
            }
            let n = n.max(1);
            let o = (y * w + x) * 3;
            out[o] = (r / n) as u8;
            out[o + 1] = (g / n) as u8;
            out[o + 2] = (b / n) as u8;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A gradient, encoded: the bytes are a JPEG and the size is the size
    /// that went in, because it is under the cap.
    #[test]
    fn a_small_picture_is_encoded_where_it_lies() {
        let (w, h) = (64usize, 48usize);
        let mut rgb = vec![0u8; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 3;
                rgb[i] = (x * 4) as u8;
                rgb[i + 1] = (y * 5) as u8;
                rgb[i + 2] = 128;
            }
        }
        let jpg = of_rgb(&rgb, w, h, PHOTO_MAX).expect("a jpeg");
        assert_eq!(&jpg[..2], b"\xff\xd8", "the start of every JPEG");
        assert_eq!(&jpg[jpg.len() - 2..], b"\xff\xd9", "and its end");
        // The dimensions live in the frame header, which is where a reader
        // finds them: SOF0 carries height then width, big endian.
        let (found_w, found_h) = size_of_jpeg(&jpg).expect("a frame header");
        assert_eq!((found_w, found_h), (w, h));
    }

    /// Past the cap the long side is the cap and the shape is kept.
    #[test]
    fn a_big_picture_is_scaled_to_the_long_side() {
        let (w, h) = (2000usize, 1000usize);
        let rgb = vec![200u8; w * h * 3];
        let jpg = of_rgb(&rgb, w, h, PHOTO_MAX).expect("a jpeg");
        assert_eq!(size_of_jpeg(&jpg), Some((1280, 640)));

        // And a square thumbnail stays square.
        let square = vec![90u8; 800 * 800 * 3];
        let thumb = of_rgb(&square, 800, 800, THUMBNAIL).expect("a jpeg");
        assert_eq!(size_of_jpeg(&thumb), Some((THUMBNAIL, THUMBNAIL)));
        assert!(thumb.len() < 64 * 1024, "a poster is small: {}", thumb.len());
    }

    /// The average is an average: a picture of two halves scaled to two
    /// pixels keeps the two halves rather than picking one.
    #[test]
    fn the_scaling_averages_what_it_covers() {
        let (w, h) = (4usize, 2usize);
        let mut rgb = vec![0u8; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 3;
                let v = if x < 2 { 0 } else { 200 };
                rgb[i] = v;
                rgb[i + 1] = v;
                rgb[i + 2] = v;
            }
        }
        let small = box_down(&rgb, w, h, 2, 1);
        assert_eq!(small, vec![0, 0, 0, 200, 200, 200]);
    }

    /// Nonsense in, an answer in words out.
    #[test]
    fn a_picture_that_is_not_one_is_refused() {
        assert!(of_rgb(&[], 0, 0, PHOTO_MAX).is_err());
        assert!(of_rgb(&[1, 2, 3], 4, 4, PHOTO_MAX).is_err());
    }

    /// The width and height a JPEG's frame header declares.
    fn size_of_jpeg(bytes: &[u8]) -> Option<(usize, usize)> {
        let mut i = 2;
        while i + 9 < bytes.len() {
            if bytes[i] != 0xff {
                return None;
            }
            let marker = bytes[i + 1];
            let len = usize::from(u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]));
            if (0xc0..=0xcf).contains(&marker) && marker != 0xc4 && marker != 0xc8 && marker != 0xcc
            {
                let h = usize::from(u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]));
                let w = usize::from(u16::from_be_bytes([bytes[i + 7], bytes[i + 8]]));
                return Some((w, h));
            }
            i += 2 + len;
        }
        None
    }
}
