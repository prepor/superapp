//! A place on a map: Web Mercator tiles around a point, composed into one
//! picture with a pin at its centre.
//!
//! The tiles come from the kernel's [`Tiles`] — its street grid where
//! nothing else is installed, and [`tiles`](super::super::tiles)'s
//! OpenStreetMap on a run a person is looking at. What is here is the maths
//! and the pin, and the three ways out to somebody else's map. Nothing here
//! is about any app.

use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::sync::Arc;

use kernel::caps::tiles::{Tile, Tiles, GROUND, TILE};

/// The zoom a snapshot of a place is taken at: streets, a few hundred
/// metres across.
pub const ZOOM: u32 = 15;

/// One composed picture: BGRA pixels, row-major, as a texture takes them.
///
/// `complete` is false where a tile was not there to draw: ground stands in
/// for it, and whoever composed the picture composes it again once the tiles
/// land.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u32>,
    pub complete: bool,
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

/// A `width`×`height` picture around `(lat, lon)`, the point at its
/// centre and a pin drawn over it. A tile the source cannot give yet is
/// left as ground, and the picture says so.
pub fn snapshot(
    src: &dyn Tiles,
    lat: f64,
    lon: f64,
    zoom: u32,
    width: usize,
    height: usize,
) -> Snapshot {
    let (cx, cy) = world_pixel(lat, lon, zoom);
    let left = cx - width as f64 / 2.0;
    let top = cy - height as f64 / 2.0;
    let edge = 1u32 << zoom;
    let mut tiles: HashMap<(u32, u32), Option<Arc<[u32]>>> = HashMap::new();
    let mut pixels = vec![GROUND; width * height];
    let mut complete = true;
    for y in 0..height {
        for x in 0..width {
            let (wx, wy) = (left + x as f64, top + y as f64);
            if wx < 0.0 || wy < 0.0 {
                continue;
            }
            let (tx, ty) = tile_of(wx, wy);
            // Past the edge of the world there is no tile to wait for.
            if tx >= edge || ty >= edge {
                continue;
            }
            let tile = match tiles.entry((tx, ty)) {
                Entry::Occupied(held) => held.into_mut(),
                Entry::Vacant(empty) => {
                    let asked = src.tile(zoom, tx, ty);
                    complete &= matches!(asked, Tile::Ready(_));
                    empty.insert(match asked {
                        Tile::Ready(pixels) => Some(pixels),
                        Tile::Pending | Tile::Missing => None,
                    })
                }
            };
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
        complete,
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

/// Apple Maps, at the point.
#[must_use]
pub fn maps_url(lat: f64, lon: f64) -> String {
    format!("https://maps.apple.com/?ll={lat},{lon}&q={lat},{lon}")
}

/// Google Maps, at the point: the one every phone in the room has.
#[must_use]
pub fn google_url(lat: f64, lon: f64) -> String {
    format!("https://maps.google.com/maps?q={lat},{lon}")
}

/// OpenStreetMap in a browser, at the point — whose tiles the picture above
/// is drawn from.
#[must_use]
pub fn osm_url(lat: f64, lon: f64) -> String {
    format!("https://www.openstreetmap.org/?mlat={lat}&mlon={lon}#map={ZOOM}/{lat}/{lon}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use kernel::caps::FakeTiles;

    /// A source with a hole in it: one tile of the picture is still coming.
    struct OneShort(u32, u32);

    impl Tiles for OneShort {
        fn tile(&self, z: u32, x: u32, y: u32) -> Tile {
            if (x, y) == (self.0, self.1) {
                Tile::Pending
            } else {
                FakeTiles.tile(z, x, y)
            }
        }
    }

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
        let s = snapshot(&FakeTiles, 47.0472, 8.3164, ZOOM, 320, 160);
        assert_eq!((s.width, s.height, s.pixels.len()), (320, 160, 320 * 160));
        assert!(s.complete, "the grid always has a tile");
        assert_eq!(s.pixels[80 * 320 + 160], INK, "the pin's dot");
        assert_eq!(s.pixels[80 * 320 + 166], WHITE, "its ring");
        // The same place is the same picture twice running.
        assert_eq!(snapshot(&FakeTiles, 47.0472, 8.3164, ZOOM, 320, 160), s);
        // A different place is a different picture.
        assert_ne!(snapshot(&FakeTiles, 55.7512, 37.6184, ZOOM, 320, 160), s);
    }

    #[test]
    fn a_tile_still_coming_is_ground_and_the_picture_is_not_done() {
        let whole = snapshot(&FakeTiles, 47.0472, 8.3164, ZOOM, 320, 160);
        let (px, py) = world_pixel(47.0472, 8.3164, ZOOM);
        let (tx, ty) = tile_of(px, py);
        let waiting = snapshot(&OneShort(tx, ty), 47.0472, 8.3164, ZOOM, 320, 160);
        assert!(!waiting.complete, "a pending tile leaves the picture unfinished");
        assert!(waiting.pixels.contains(&GROUND), "ground where it goes");
        assert_ne!(waiting, whole);
        // And once it lands, the picture is the whole one again.
        assert_eq!(snapshot(&FakeTiles, 47.0472, 8.3164, ZOOM, 320, 160), whole);
    }

    #[test]
    fn the_urls_carry_the_point() {
        assert_eq!(
            maps_url(47.0472, 8.3164),
            "https://maps.apple.com/?ll=47.0472,8.3164&q=47.0472,8.3164"
        );
        assert_eq!(
            google_url(47.0472, 8.3164),
            "https://maps.google.com/maps?q=47.0472,8.3164"
        );
        assert!(osm_url(47.0472, 8.3164).ends_with("#map=15/47.0472/8.3164"));
    }
}
