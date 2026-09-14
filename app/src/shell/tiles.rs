//! OpenStreetMap's raster tiles, fetched: the real map under a place.
//!
//! It is here rather than in `platform/` because none of it is what *this
//! machine* answers for — there is no macOS way and android way to fetch a
//! PNG. It is the shell's own map made real, beside the maths in
//! [`widgets::map`](super::widgets::map): one client, one cache, the same
//! code on every build.
//!
//! The policy is OpenStreetMap's, not ours, and the whole of this module is
//! written to keep it:
//!
//! - the program names itself, its version and its repository in the user
//!   agent, so a tile server knows who is asking;
//! - at most two fetches are on the wire at a time;
//! - what has been fetched is kept — the PNG in the kernel's blob cache
//!   under `tile:<z>/<x>/<y>`, so a restart draws the same map without
//!   asking again, and the decoded pixels in a small LRU, so composing a
//!   picture is copying;
//! - a fetch that failed is not tried again for a minute.
//!
//! A tile that is not held answers [`Tile::Pending`] and is fetched; when it
//! lands, [`Landed`] wakes whoever drew ground where it goes, the way a
//! decoded picture does. [`boot`](super::boot) installs this on a run nobody
//! is scripting — a suite and the panels library draw the kernel's street
//! grid and touch no network at all.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kernel::caps::tiles::{Tile, Tiles, TILE};
use kernel::caps::{BlobCache, Blobs};
use makepad_widgets::image_cache::decode_image_from_data;
use makepad_widgets::{Cx, Event};

/// Where the tiles are.
const SERVER: &str = "https://tile.openstreetmap.org";

/// Who is asking. The tile policy wants a program to name itself and say
/// where it lives, so that whoever runs the servers can reach its author.
const AGENT: &str = concat!(
    "superapp/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/prepor/superapp)"
);

/// How many decoded tiles are kept: a quarter of a megabyte each, so this
/// is sixteen megabytes — a few screens of map at once.
const HELD: usize = 64;

/// A tile is a small picture; a megabyte of one is not a tile.
const MAX_TILE: usize = 1 << 20;

/// On the wire at a time, as the policy asks.
const AT_ONCE: usize = 2;

/// How long a tile the wire refused is left alone. A map is not worth
/// hammering a free service for.
const COOL_OFF: Duration = Duration::from_secs(60);

/// How long one tile may take before it counts as refused.
const PATIENCE: Duration = Duration::from_secs(10);

/// A tile, by zoom and position.
type Key = (u32, u32, u32);

/// Where a tile's PNG is kept. The blob cache is keyed by whoever owns the
/// bytes, and a map's tiles are the shell's own.
#[must_use]
pub fn key(z: u32, x: u32, y: u32) -> String {
    format!("tile:{z}/{x}/{y}")
}

/// A tile has landed. A map drawn with ground where it goes can be drawn
/// again: the transcript's picture cache and the place panel both listen for
/// this, as they listen for a decoded picture.
#[derive(Debug)]
pub struct Landed;

/// Whether this event says a tile landed.
#[must_use]
pub fn landed(event: &Event) -> bool {
    matches!(event, Event::Actions(actions) if actions.iter().any(|a| a.downcast_ref::<Landed>().is_some()))
}

/// What goes to the wire for a tile. The web is one implementation; a test
/// hands over bytes it already has, because nothing here may reach the
/// network under a test.
#[async_trait::async_trait]
pub trait Fetch: Send + Sync {
    /// The bytes at `url`, or why not.
    async fn get(&self, url: &str) -> Result<Vec<u8>, String>;
}

/// OpenStreetMap, as a tile source: a blob cache under it, an LRU of
/// decoded tiles over it, and two fetches at a time between them.
#[derive(Clone)]
pub struct Osm(Arc<Inner>);

impl Osm {
    /// The real one: tiles over HTTPS, kept in `blobs`.
    #[must_use]
    pub fn web(blobs: BlobCache) -> Osm {
        Osm::over(blobs, Arc::new(Web(client())))
    }

    /// The same source over another way of getting bytes — what a test uses
    /// to land a tile without a network.
    #[must_use]
    pub fn over(blobs: BlobCache, fetch: Arc<dyn Fetch>) -> Osm {
        Osm(Arc::new(Inner {
            state: Mutex::new(State::default()),
            blobs,
            fetch,
            gate: tokio::sync::Semaphore::new(AT_ONCE),
        }))
    }
}

impl std::fmt::Debug for Osm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Osm")
    }
}

impl Tiles for Osm {
    fn tile(&self, z: u32, x: u32, y: u32) -> Tile {
        let asked = (z, x, y);
        let Ok(mut state) = self.0.state.lock() else {
            return Tile::Missing;
        };
        if let Some(pixels) = state.held.take(asked) {
            return Tile::Ready(pixels);
        }
        if state.fetching.contains(&asked) {
            return Tile::Pending;
        }
        if state.failed.get(&asked).is_some_and(|at| at.elapsed() < COOL_OFF) {
            return Tile::Missing;
        }
        state.failed.remove(&asked);
        state.fetching.insert(asked);
        drop(state);
        let inner = self.0.clone();
        kernel::runtime::spawn(async move { inner.land(asked).await });
        Tile::Pending
    }
}

/// The one backend behind every clone: what is held, what is being fetched,
/// and what the wire refused.
struct Inner {
    state: Mutex<State>,
    blobs: BlobCache,
    fetch: Arc<dyn Fetch>,
    gate: tokio::sync::Semaphore,
}

#[derive(Default)]
struct State {
    held: Held,
    /// On its way: whoever asks next waits rather than fetching it again.
    fetching: HashSet<Key>,
    /// What the wire refused, and when.
    failed: HashMap<Key, Instant>,
}

impl Inner {
    /// Fetches, decodes and files one tile, then wakes the maps waiting for
    /// it. A failure is recorded with its hour, so the next ask is refused
    /// rather than sent.
    async fn land(self: Arc<Self>, asked: Key) {
        let got = self.decoded(asked).await;
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        state.fetching.remove(&asked);
        match got {
            Ok(pixels) => {
                state.held.put(asked, pixels);
                drop(state);
                Cx::post_action(Landed);
            }
            Err(error) => {
                state.failed.insert(asked, Instant::now());
                drop(state);
                let (z, x, y) = asked;
                eprintln!("map: tile {z}/{x}/{y}: {error}");
            }
        }
    }

    /// One tile's pixels: its bytes, decoded off both executors, and only
    /// if they really are a tile.
    async fn decoded(&self, asked: Key) -> Result<Arc<[u32]>, String> {
        let bytes = self.bytes(asked).await?;
        let image = kernel::runtime::spawn_blocking(move || {
            decode_image_from_data(&bytes).map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())??;
        if image.width != TILE || image.height != TILE {
            return Err(format!(
                "a tile is {TILE}×{TILE}; this one is {}×{}",
                image.width, image.height
            ));
        }
        Ok(image.data.into())
    }

    /// A tile's PNG: the cached file where there is one — so a restart never
    /// refetches — and the wire otherwise, which is what the gate bounds.
    async fn bytes(&self, asked: Key) -> Result<Vec<u8>, String> {
        let (z, x, y) = asked;
        let key = key(z, x, y);
        let mut cache = self.blobs.clone();
        let kept = {
            let key = key.clone();
            kernel::runtime::spawn_blocking(move || {
                let path = cache.get(&key)?;
                match std::fs::read(path) {
                    Ok(bytes) => Some(bytes),
                    Err(_) => {
                        cache.remove(&key);
                        None
                    }
                }
            })
            .await
            .map_err(|e| e.to_string())?
        };
        if let Some(bytes) = kept {
            return Ok(bytes);
        }
        let bytes = {
            let _on_the_wire = self
                .gate
                .acquire()
                .await
                .map_err(|_| "the tile gate is closed".to_string())?;
            self.fetch.get(&format!("{SERVER}/{z}/{x}/{y}.png")).await?
        };
        let mut cache = self.blobs.clone();
        kernel::runtime::spawn_blocking(move || {
            cache.put(&key, &bytes)?;
            Ok::<_, String>(bytes)
        })
        .await
        .map_err(|e| e.to_string())?
    }
}

/// The decoded tiles this run holds, stalest first.
#[derive(Default)]
struct Held(VecDeque<(Key, Arc<[u32]>)>);

impl Held {
    /// The tile, which is the freshest one now.
    fn take(&mut self, asked: Key) -> Option<Arc<[u32]>> {
        let at = self.0.iter().position(|(held, _)| *held == asked)?;
        let entry = self.0.remove(at)?;
        let pixels = entry.1.clone();
        self.0.push_back(entry);
        Some(pixels)
    }

    /// Keeps a tile, dropping the stalest once there are too many.
    fn put(&mut self, asked: Key, pixels: Arc<[u32]>) {
        self.0.retain(|(held, _)| *held != asked);
        self.0.push_back((asked, pixels));
        while self.0.len() > HELD {
            self.0.pop_front();
        }
    }
}

/// The wire itself.
struct Web(reqwest::Client);

#[async_trait::async_trait]
impl Fetch for Web {
    async fn get(&self, url: &str) -> Result<Vec<u8>, String> {
        let response = self
            .0
            .get(url)
            .send()
            .await
            .map_err(|e| e.without_url().to_string())?;
        if !response.status().is_success() {
            return Err(format!("the tile server answered HTTP {}", response.status()));
        }
        kernel::http::bounded_bytes(response, MAX_TILE).await
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .use_preconfigured_tls((*kernel::http::tls_config()).clone())
        .timeout(PATIENCE)
        .redirect(reqwest::redirect::Policy::limited(3))
        .retry(reqwest::retry::never())
        .user_agent(AGENT)
        .build()
        .expect("the tile HTTP client")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A 256×256 picture that is already in the build: the app's own icon,
    /// which decodes exactly as a tile does and never leaves the machine.
    const TILE_PNG: &[u8] = include_bytes!("../../resources/icon_256.png");

    /// A wire that hands over bytes it already has, and counts the asks.
    struct Canned {
        bytes: Result<Vec<u8>, String>,
        asks: AtomicUsize,
    }

    impl Canned {
        fn holding(bytes: &[u8]) -> Arc<Canned> {
            Arc::new(Canned {
                bytes: Ok(bytes.to_vec()),
                asks: AtomicUsize::new(0),
            })
        }

        fn refusing() -> Arc<Canned> {
            Arc::new(Canned {
                bytes: Err("no tile server here".to_string()),
                asks: AtomicUsize::new(0),
            })
        }

        fn asks(&self) -> usize {
            self.asks.load(Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl Fetch for Canned {
        async fn get(&self, _url: &str) -> Result<Vec<u8>, String> {
            self.asks.fetch_add(1, Ordering::SeqCst);
            self.bytes.clone()
        }
    }

    fn cache(name: &str) -> (std::path::PathBuf, BlobCache) {
        let dir = std::env::temp_dir().join(format!(
            "superapp-tiles-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        (
            dir.clone(),
            BlobCache::at(dir, kernel::caps::BLOB_BUDGET_DEFAULT),
        )
    }

    /// Asks until the tile is no longer on its way, or gives up.
    fn settle(source: &Osm, asked: Key) -> Tile {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match source.tile(asked.0, asked.1, asked.2) {
                Tile::Pending if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(1));
                }
                settled => return settled,
            }
        }
    }

    #[test]
    fn a_tile_is_named_by_its_place_in_the_world() {
        assert_eq!(key(15, 17140, 11519), "tile:15/17140/11519");
        assert_ne!(key(15, 17140, 11519), key(16, 17140, 11519));
    }

    #[test]
    fn the_held_tiles_keep_the_ones_last_asked_for() {
        let mut held = Held::default();
        let px = |n: u32| -> Arc<[u32]> { Arc::from(vec![n; 4]) };
        for n in 0..HELD as u32 {
            held.put((15, n, 0), px(n));
        }
        // Asking for the stalest makes it the freshest, so the next one out
        // is the tile behind it.
        assert!(held.take((15, 0, 0)).is_some());
        held.put((15, HELD as u32, 0), px(999));
        assert!(held.take((15, 1, 0)).is_none(), "the stalest went");
        assert!(held.take((15, 0, 0)).is_some(), "the one asked for stayed");
        assert_eq!(held.0.len(), HELD);
    }

    #[test]
    fn a_tile_is_pending_while_it_is_fetched_and_ready_once_it_lands() {
        let (dir, blobs) = cache("lands");
        let wire = Canned::holding(TILE_PNG);
        let source = Osm::over(blobs.clone(), wire.clone());
        assert!(matches!(source.tile(15, 17140, 11519), Tile::Pending));
        let Tile::Ready(pixels) = settle(&source, (15, 17140, 11519)) else {
            panic!("the tile never landed");
        };
        assert_eq!(pixels.len(), TILE * TILE);
        assert_eq!(wire.asks(), 1, "one fetch, however many asks");
        // Held, so the next ask is a copy rather than a fetch.
        assert!(matches!(source.tile(15, 17140, 11519), Tile::Ready(_)));
        assert_eq!(wire.asks(), 1);
        // And the PNG is in the cache, so the next run of the app has it.
        assert!(blobs.clone().contains(&key(15, 17140, 11519)));
        let restarted = Osm::over(blobs, Canned::refusing());
        assert!(matches!(settle(&restarted, (15, 17140, 11519)), Tile::Ready(_)),
            "a restart draws the map without asking the servers again");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_refused_tile_is_missing_and_is_left_alone_for_a_while() {
        let (dir, blobs) = cache("refused");
        let wire = Canned::refusing();
        let source = Osm::over(blobs, wire.clone());
        assert!(matches!(source.tile(15, 1, 1), Tile::Pending));
        assert!(matches!(settle(&source, (15, 1, 1)), Tile::Missing));
        assert!(matches!(source.tile(15, 1, 1), Tile::Missing));
        assert_eq!(wire.asks(), 1, "a refusal is not asked again at once");
        let _ = std::fs::remove_dir_all(dir);
    }
}
