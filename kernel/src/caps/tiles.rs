//! Map tiles: the squares the world is drawn in.
//!
//! Web Mercator cuts the world into 256-pixel squares — one for every
//! `(z, x, y)` — and every raster server serves the same ones, so what a map
//! asks for is a tile, and what comes back is pixels, *not yet*, or *not at
//! all*. The kernel owns the question for the reason it owns the voice: the
//! answer is outside this process, and the fake has to be one fake. That
//! fake is a drawn street grid, the same for a tile every time, so the
//! panels library and an e2e suite draw the same map for the same place on
//! any machine, with no network and nothing cached.
//!
//! The real source is the shell's — OpenStreetMap's raster tiles, fetched,
//! kept as blobs and decoded there. It is installed twice: into the window's
//! world, the way the other real capabilities are, and as the one
//! process-wide [`source`], because a map is composed on a picture worker
//! that holds no world at all.

use std::sync::{Arc, OnceLock};

/// A tile's side, in pixels: the square every raster server serves.
pub const TILE: usize = 256;

/// The ground a map stands on: what shows wherever a tile has not come.
pub const GROUND: u32 = 0xffe7_e7e7;

/// What a source answers when a map asks it for a tile.
#[derive(Clone, Debug)]
pub enum Tile {
    /// [`TILE`]² BGRA pixels, row-major, as a texture takes them.
    Ready(Arc<[u32]>),
    /// Asked for and on its way: draw ground there, and ask again when it
    /// lands.
    Pending,
    /// Not to be had — the fetch failed, or there is no such tile.
    Missing,
}

/// Where tiles come from.
///
/// Shared across threads, and answered without waiting: a map is composed on
/// a worker, so a source that has to go to the wire starts the fetch and
/// says [`Tile::Pending`] rather than holding the picture up.
pub trait Tiles: Send + Sync {
    /// Tile `(z, x, y)`.
    fn tile(&self, z: u32, x: u32, y: u32) -> Tile;
}

/// A shared source is a source: what [`source`] hands out is an `Arc`, and it
/// goes into a capability bag as it stands.
impl<T: Tiles + ?Sized> Tiles for Arc<T> {
    fn tile(&self, z: u32, x: u32, y: u32) -> Tile {
        (**self).tile(z, x, y)
    }
}

static SOURCE: OnceLock<Arc<dyn Tiles>> = OnceLock::new();

/// Installs the source this run's maps draw from. The first call wins: there
/// is one machine, one cache and one tile policy, however many worlds a run
/// builds. The shell calls it once, at boot, and only on a run nobody is
/// scripting — so a suite goes on drawing the grid below.
pub fn install(source: Arc<dyn Tiles>) {
    let _ = SOURCE.set(source);
}

/// The source to draw a map from where there is no world to ask — which is
/// where maps are drawn: on a picture worker, off any `Cx`. The fake until
/// [`install`] has been called.
#[must_use]
pub fn source() -> Arc<dyn Tiles> {
    static FAKE: OnceLock<Arc<dyn Tiles>> = OnceLock::new();
    SOURCE
        .get()
        .unwrap_or_else(|| FAKE.get_or_init(|| Arc::new(FakeTiles)))
        .clone()
}

const WHITE: u32 = 0xffff_ffff;
const BLOCK: u32 = 0xffdc_dcdc;
const PARK: u32 = 0xffd0_d0d0;

/// A drawn street grid, deterministic per tile: the map a world with no real
/// source shows, and what a suite photographs.
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

impl Tiles for FakeTiles {
    fn tile(&self, z: u32, x: u32, y: u32) -> Tile {
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
        Tile::Ready(px.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready(t: Tile) -> Arc<[u32]> {
        match t {
            Tile::Ready(px) => px,
            _ => panic!("the fake always has a tile"),
        }
    }

    #[test]
    fn the_grid_is_one_picture_per_tile_and_another_one_next_door() {
        let one = ready(FakeTiles.tile(15, 17140, 11519));
        assert_eq!(one.len(), TILE * TILE);
        assert_eq!(one, ready(FakeTiles.tile(15, 17140, 11519)));
        assert_ne!(one, ready(FakeTiles.tile(15, 17141, 11519)));
        assert!(one.contains(&WHITE), "streets run through it");
    }

    #[test]
    fn a_run_that_installed_nothing_draws_the_grid() {
        assert!(matches!(source().tile(15, 1, 1), Tile::Ready(_)));
    }
}
