//! A place on a map: Web Mercator tiles around a point, composed into one
//! picture with a pin at its centre.
//!
//! The tiles come from a [`TileSource`]. The kernel will own the real one —
//! OpenStreetMap's raster tiles, fetched by a worker and cached under the
//! app's directory — and this module owns the maths and the fake: a drawn
//! street grid, deterministic per tile, so a suite and the library show the
//! same map for the same place. Nothing here is about any app.

use std::collections::HashMap;

/// A tile's side, in pixels.
pub const TILE: usize = 256;

/// The zoom a snapshot of a place is taken at: streets, a few hundred
/// metres across.
pub const ZOOM: u32 = 15;

/// One composed picture: BGRA pixels, row-major, as a texture takes them.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u32>,
}

/// Where tiles come from.
pub trait TileSource {
    /// Tile `(z, x, y)` as [`TILE`]² BGRA pixels, or `None` where it cannot
    /// be had right now.
    fn tile(&mut self, z: u32, x: u32, y: u32) -> Option<Vec<u32>>;
}

/// A point's position on the world at a zoom, in pixels from the top-left
/// of tile (0, 0): the Web Mercator projection every raster tile server
/// uses.
#[must_use]
pub fn world_pixel(lat: f64, lon: f64, zoom: u32) -> (f64, f64) {
    let n = f64::from(1u32 << zoom) * TILE as f64;
    let x = (lon + 180.0) / 360.0 * n;
    let lat = lat.clamp(-85.051_13, 85.051_13).to_radians();
    let y = (1.0 - (lat.tan() + 1.0 / lat.cos()).ln() / std::f64::consts::PI) / 2.0 * n;
    (x, y)
}

/// The tile a world pixel falls in.
#[must_use]
pub fn tile_of(px: f64, py: f64) -> (u32, u32) {
    ((px / TILE as f64).floor() as u32, (py / TILE as f64).floor() as u32)
}

const INK: u32 = 0xff14_1414;
const WHITE: u32 = 0xffff_ffff;
const GROUND: u32 = 0xffe7_e7e7;
const BLOCK: u32 = 0xffdc_dcdc;
const PARK: u32 = 0xffd0_d0d0;

/// A `width`×`height` picture around `(lat, lon)`, the point at its
/// centre and a pin drawn over it. A tile the source cannot give is left
/// as ground.
pub fn snapshot(
    src: &mut dyn TileSource,
    lat: f64,
    lon: f64,
    zoom: u32,
    width: usize,
    height: usize,
) -> Snapshot {
    let (cx, cy) = world_pixel(lat, lon, zoom);
    let left = cx - width as f64 / 2.0;
    let top = cy - height as f64 / 2.0;
    let mut tiles: HashMap<(u32, u32), Option<Vec<u32>>> = HashMap::new();
    let mut pixels = vec![GROUND; width * height];
    for y in 0..height {
        for x in 0..width {
            let (wx, wy) = (left + x as f64, top + y as f64);
            if wx < 0.0 || wy < 0.0 {
                continue;
            }
            let (tx, ty) = tile_of(wx, wy);
            let tile = tiles
                .entry((tx, ty))
                .or_insert_with(|| src.tile(zoom, tx, ty));
            let Some(t) = tile else { continue };
            let (ix, iy) = (
                (wx - f64::from(tx) * TILE as f64) as usize,
                (wy - f64::from(ty) * TILE as f64) as usize,
            );
            if let Some(p) = t.get(iy * TILE + ix) {
                pixels[y * width + x] = *p;
            }
        }
    }
    pin(&mut pixels, width, height);
    Snapshot {
        width,
        height,
        pixels,
    }
}

/// The pin: an ink disc in a white ring, at the centre.
fn pin(pixels: &mut [u32], width: usize, height: usize) {
    let (cx, cy) = (width as f64 / 2.0, height as f64 / 2.0);
    for y in 0..height {
        for x in 0..width {
            let d = ((x as f64 + 0.5 - cx).powi(2) + (y as f64 + 0.5 - cy).powi(2)).sqrt();
            let p = &mut pixels[y * width + x];
            if d < 5.0 {
                *p = INK;
            } else if d < 7.5 {
                *p = WHITE;
            } else if d < 8.5 {
                *p = INK;
            }
        }
    }
}

/// A drawn street grid, deterministic per tile: the map a world with no
/// tiles shows, and what a suite photographs.
#[derive(Debug, Default, Clone, Copy)]
pub struct FakeTiles;

/// A small integer hash, for the streets' positions.
fn hash(mut v: u64) -> u64 {
    v ^= v >> 33;
    v = v.wrapping_mul(0xff51_afd7_ed55_8ccd);
    v ^= v >> 33;
    v = v.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    v ^= v >> 33;
    v
}

impl TileSource for FakeTiles {
    fn tile(&mut self, z: u32, x: u32, y: u32) -> Option<Vec<u32>> {
        let mut px = vec![GROUND; TILE * TILE];
        // Blocks: a lightly darker fill on a coarse grid, so the streets
        // between them read as streets.
        let seed = hash((u64::from(z) << 40) | (u64::from(x) << 20) | u64::from(y));
        let mut streets_x: Vec<usize> = Vec::new();
        let mut streets_y: Vec<usize> = Vec::new();
        let mut at = 0usize;
        let mut k = 0u64;
        while at < TILE {
            let step = 34 + (hash(seed ^ k) % 40) as usize;
            k += 1;
            at += step;
            if at < TILE {
                streets_x.push(at);
            }
        }
        at = 0;
        while at < TILE {
            let step = 30 + (hash(seed.rotate_left(17) ^ k) % 44) as usize;
            k += 1;
            at += step;
            if at < TILE {
                streets_y.push(at);
            }
        }
        // One park a tile, sometimes.
        let park = hash(seed ^ 0xbeef).is_multiple_of(3).then(|| {
            let px0 = (hash(seed ^ 0x11) % 180) as usize;
            let py0 = (hash(seed ^ 0x22) % 180) as usize;
            (px0, py0, px0 + 50 + (hash(seed ^ 0x33) % 40) as usize, py0 + 40 + (hash(seed ^ 0x44) % 40) as usize)
        });
        for yy in 0..TILE {
            for xx in 0..TILE {
                let mut c = BLOCK;
                if let Some((x0, y0, x1, y1)) = park {
                    if xx >= x0 && xx < x1 && yy >= y0 && yy < y1 {
                        c = PARK;
                    }
                }
                let on_street = streets_x.iter().any(|s| xx + 1 >= *s && xx < s + 3)
                    || streets_y.iter().any(|s| yy + 1 >= *s && yy < s + 3);
                if on_street {
                    c = WHITE;
                }
                px[yy * TILE + xx] = c;
            }
        }
        Some(px)
    }
}

/// Apple Maps, at the point.
#[must_use]
pub fn maps_url(lat: f64, lon: f64) -> String {
    format!("https://maps.apple.com/?ll={lat},{lon}&q={lat},{lon}")
}

/// OpenStreetMap in a browser, at the point.
#[must_use]
pub fn osm_url(lat: f64, lon: f64) -> String {
    format!("https://www.openstreetmap.org/?mlat={lat}&mlon={lon}#map={ZOOM}/{lat}/{lon}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_projection_puts_greenwich_in_the_middle_and_north_up() {
        let (x, y) = world_pixel(0.0, 0.0, 0);
        assert!((x - 128.0).abs() < 1e-6 && (y - 128.0).abs() < 1e-6);
        let (x, y) = world_pixel(51.5, -0.12, 0);
        assert!(x < 128.0 && y < 128.0, "north-west of the origin is up and left");
        let (px, py) = world_pixel(47.0472, 8.3164, ZOOM);
        assert_eq!(tile_of(px, py), (17140, 11519));
    }

    #[test]
    fn a_snapshot_is_the_size_asked_for_with_the_pin_in_the_middle() {
        let s = snapshot(&mut FakeTiles, 47.0472, 8.3164, ZOOM, 320, 160);
        assert_eq!((s.width, s.height, s.pixels.len()), (320, 160, 320 * 160));
        assert_eq!(s.pixels[80 * 320 + 160], INK, "the pin's dot");
        assert_eq!(s.pixels[80 * 320 + 166], WHITE, "its ring");
        // The same place is the same picture twice running.
        assert_eq!(snapshot(&mut FakeTiles, 47.0472, 8.3164, ZOOM, 320, 160), s);
        // A different place is a different picture.
        assert_ne!(snapshot(&mut FakeTiles, 55.7512, 37.6184, ZOOM, 320, 160), s);
    }

    #[test]
    fn the_urls_carry_the_point() {
        assert_eq!(
            maps_url(47.0472, 8.3164),
            "https://maps.apple.com/?ll=47.0472,8.3164&q=47.0472,8.3164"
        );
        assert!(osm_url(47.0472, 8.3164).ends_with("#map=15/47.0472/8.3164"));
    }
}
