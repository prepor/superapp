//! The pictures an open letter shows, and the item that draws one.
//!
//! Nothing here happens in the frame that first shows a picture. A letter's
//! own `cid:` parts come from the local file cache or IMAP on a reader thread
//! with the requesting world's capabilities; a `data:` payload is decoded on
//! that same thread; an image on the web is an ordinary HTTP request. All three
//! land in [`landed`], which redraws. The decode from those bytes to a
//! texture is makepad's, on its own pool, and lands there too.
//!
//! [`Pictures`] lives on `Cx` as a global, because the image items are minted
//! by the `Html` widget from a template and can reach nothing else. The names
//! are one flat space over every store a process has open — the panels
//! library's business alone, since no scene of the catalogue has a `cid:`
//! picture in it.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{mpsc, Arc};

use kernel::effect::World;
use kernel::store::Db;
use makepad_widgets::image_cache::{
    looks_like_svg, process_async_image_load, AsyncImageLoad, AsyncLoadResult, ImageCacheImpl,
};
use makepad_widgets::*;

use super::super::html;
use super::super::model::MailId;
use super::super::parts;

/// The bytes of every image an open letter refers to, by the name [`key`]
/// files it under — fetched, read or un-base64'd by whoever has them, and
/// read back by [`HtmlImage`] as it draws.
#[derive(Default)]
pub struct Pictures {
    bytes: HashMap<String, Arc<[u8]>>,
    /// Requests out on the network, by request id.
    inflight: HashMap<LiveId, String>,
    /// Sources that did not arrive or did not decode: asked once, not again.
    failed: HashSet<String>,
    /// Jobs handed to the reader thread: inline parts, file previews, and
    /// `data:` sources. Deduplicated while in flight.
    asked: HashSet<String>,
    /// Download failures can recover after connectivity or server location
    /// changes. Retry on a later draw, with a delay to avoid a request loop.
    retry_at: HashMap<String, std::time::Instant>,
    /// The reader thread, started with the first letter that has a picture
    /// in it.
    reader: Option<mpsc::Sender<Job>>,
    /// Where the pictures that stood in a link were drawn, this draw. An
    /// item is minted from a template and can reach neither the panel around
    /// it nor the hit table, so it leaves its rectangle here and the reader
    /// takes it (see [`link_rects`]).
    links: Vec<Rect>,
}

/// File retrieval and data-URL decoding, off the UI thread.
enum Job {
    /// Download or read one letter's inline images.
    ///
    /// The database comes with the job rather than being held here: a
    /// panels-library mount boots a stage over a world of its own, and a
    /// reader that had bound one database at startup would answer every later
    /// panel out of whichever store happened to ask first.
    Cid {
        db: Arc<Db>,
        reader: parts::Reader,
        mid: MailId,
        key: String,
    },
    /// Un-base64 one `data:` source, filed under `key`.
    Data { key: String, src: String },
    /// Download or read one file for its card's preview.
    Part {
        db: Arc<Db>,
        reader: parts::Reader,
        mail: MailId,
        at: u32,
        key: String,
    },
}

/// What the reader thread found, on its way back to the UI thread.
struct Ready {
    items: Vec<(String, Arc<[u8]>)>,
    failed: Vec<String>,
    /// Jobs to forget having asked: the work found nothing, and the reason
    /// may not last — a letter whose raw has not been stored yet is worth
    /// asking about again the next time it opens.
    retry: Vec<String>,
}

impl std::fmt::Debug for Ready {
    /// The bytes themselves are megabytes and never worth printing; what an
    /// action log wants is which sources landed.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ready")
            .field(
                "items",
                &self.items.iter().map(|(k, _)| k).collect::<Vec<_>>(),
            )
            .field("failed", &self.failed)
            .field("retry", &self.retry)
            .finish()
    }
}

/// The name a source is filed under: its own, unless it carries its bytes
/// inside it. A `data:` URL *is* its payload — megabytes of map key, and of
/// texture-cache path, for what sixteen hex characters name just as well.
fn key(src: &str) -> String {
    if src.starts_with("data:") {
        format!("data:{:016x}", LiveId::from_str(src).0)
    } else {
        src.to_string()
    }
}

/// The name one part of a letter is filed under — the same flat space a
/// picture's source lives in, since both are "bytes a panel needs and must
/// not read in its own frame".
fn part_key(scope: &str, at: u32) -> String {
    format!("part:{scope}/{at}")
}

impl Pictures {
    fn retry_due(&mut self, key: &str) {
        if self
            .retry_at
            .get(key)
            .is_some_and(|at| *at <= std::time::Instant::now())
        {
            self.retry_at.remove(key);
            self.asked.remove(key);
            self.failed.remove(key);
        }
    }
    /// The reader thread, started on first need. `None` under
    /// `MAKEPAD=headless`, where the caller does the work in the frame — a
    /// scripted run wants its pictures in the frame that drew them, which is
    /// the same bargain makepad's own decode strikes under that cfg.
    fn reader(&mut self) -> Option<mpsc::Sender<Job>> {
        // `cfg!` rather than `#[cfg]` so the thread and its jobs stay
        // compiled under headless: the branch folds away either way, and code
        // the linter can still see is code that cannot rot.
        if cfg!(headless) {
            return None;
        }
        if self.reader.is_none() {
            self.reader = Some(spawn());
        }
        self.reader.clone()
    }

    /// Files what the reader thread (or, with no thread, the frame) found.
    fn take(&mut self, ready: &Ready) {
        for (k, bytes) in &ready.items {
            self.bytes.insert(k.clone(), bytes.clone());
            self.failed.remove(k);
            self.retry_at.remove(k);
        }
        for k in &ready.failed {
            self.failed.insert(k.clone());
        }
        for k in &ready.retry {
            self.retry_at.insert(
                k.clone(),
                std::time::Instant::now() + std::time::Duration::from_secs(30),
            );
        }
    }
}

/// Download, read and decode bytes, then return them as an action. Each
/// requesting store gets its own world and IMAP session on this thread.
///
/// # Panics
///
/// If the thread cannot be spawned.
fn spawn() -> mpsc::Sender<Job> {
    let (tx, rx) = mpsc::channel::<Job>();
    std::thread::Builder::new()
        .name("pictures".into())
        .spawn(move || {
            // Whichever *one* writer the job names, kept for as long as the
            // jobs keep naming it — one process can have several worlds open
            // at once (the panels library), and in every other run this opens
            // exactly once.
            let mut held: Option<(Arc<Db>, World)> = None;
            fn hold(
                held: &mut Option<(Arc<Db>, World)>,
                db: Arc<Db>,
                reader: parts::Reader,
            ) -> Option<&World> {
                if !held.as_ref().is_some_and(|(h, _)| Arc::ptr_eq(h, &db)) {
                    *held = reader.world(db.clone()).ok().map(|w| (db, w));
                }
                held.as_ref().map(|(_, s)| s)
            }
            while let Ok(job) = rx.recv() {
                let ready = match job {
                    Job::Cid {
                        db,
                        reader,
                        mid,
                        key,
                    } => cid_parts(hold(&mut held, db, reader), mid, key),
                    Job::Part {
                        db,
                        reader,
                        mail,
                        at,
                        key,
                    } => letter_part(hold(&mut held, db, reader), mail, at, key),
                    Job::Data { key, src } => data_bytes(key, &src),
                };
                Cx::post_action(ready);
            }
        })
        .expect("spawn the picture reader");
    tx
}

/// One letter's inline images, under the names used by `html::scope_cids`.
fn cid_parts(world: Option<&World>, mid: MailId, scope: String) -> Ready {
    let mut items = Vec::new();
    let mut failed = Vec::new();
    if let Some(w) = world {
        if let Some(content) = super::super::model::raw(w.store(), mid)
            .and_then(|raw| super::super::content::Content::read(&raw).ok())
        {
            for p in content
                .parts
                .iter()
                .filter(|p| !p.part.cid.is_empty() && p.part.mime.starts_with("image/"))
            {
                let key = format!("cid:{scope}/{}", p.part.cid);
                match parts::download(w, mid, p) {
                    Ok(bytes) => items.push((key, Arc::from(bytes))),
                    Err(_) => failed.push(key),
                }
            }
        }
    }
    // A letter with no raw stored for it yet has nothing to take apart — and
    // may well have it by the next time it opens, so the ask is not held
    // against it.
    let retry = if items.is_empty() || !failed.is_empty() {
        vec![scope]
    } else {
        Vec::new()
    };
    Ready {
        items,
        failed,
        retry,
    }
}

/// One file preview. Failed downloads can retry after a delay; successful
/// previews keep only as much data as the card can display.
fn letter_part(world: Option<&World>, mail: MailId, at: u32, k: String) -> Ready {
    let bytes = world
        .and_then(|w| parts::attachment(w.store(), mail, at).map(|a| (w, a)))
        .and_then(|(w, a)| parts::part(w, &a).ok());
    match bytes {
        // Cut to the preview's own ceiling before it is *kept*: this cache
        // outlives the card, and a card only ever draws the first
        // `IMAGE_PREVIEW_MAX` of a part anyway. What `open` hands to the OS
        // does not come through here — it reads the whole part and writes it
        // out (see `Card::write_out`).
        Some(b) => Ready {
            items: vec![(
                k,
                Arc::from(&b[..b.len().min(kernel::caps::IMAGE_PREVIEW_MAX)]),
            )],
            failed: Vec::new(),
            retry: Vec::new(),
        },
        None => Ready {
            items: Vec::new(),
            failed: vec![k.clone()],
            retry: vec![k],
        },
    }
}

/// The bytes a `data:` source carries, un-base64'd.
fn data_bytes(k: String, src: &str) -> Ready {
    match src
        .strip_prefix("data:")
        .and_then(|rest| rest.split_once(','))
        .and_then(|(_, b)| html::base64_decode(b))
    {
        Some(bytes) => Ready {
            items: vec![(k, Arc::from(bytes))],
            failed: Vec::new(),
            retry: Vec::new(),
        },
        None => Ready {
            items: Vec::new(),
            failed: vec![k],
            retry: Vec::new(),
        },
    }
}

/// What asking for a part's bytes answers.
pub enum PartBytes {
    Here(Arc<[u8]>),
    /// The reader has it and has not answered yet — hold the card open.
    Coming,
    /// It cannot be had: the letter no longer yields that part. Said once, so
    /// the card can stop waiting and say so.
    Gone,
}

/// Asks for one part's bytes, once, and answers with them when they are here.
/// The card calls this every draw: asking is one lookup, and the answer
/// arrives through [`landed`], which redraws.
pub fn want_part(cx: &mut Cx, world: &World, mail: MailId, at: u32) -> PartBytes {
    let k = part_key(&parts::image_scope(world.store(), mail), at);
    let p = cx.global::<Pictures>();
    if let Some(b) = p.bytes.get(&k) {
        return PartBytes::Here(b.clone());
    }
    p.retry_due(&k);
    if p.failed.contains(&k) {
        return PartBytes::Gone;
    }
    if !p.asked.insert(k.clone()) {
        return PartBytes::Coming;
    }
    if let Some(tx) = p.reader() {
        let Ok(reader) = world.with_cap::<parts::Reader, _>(|r| r.clone()) else {
            return PartBytes::Gone;
        };
        let _ = tx.send(Job::Part {
            db: world.store().db(),
            reader,
            mail,
            at,
            key: k,
        });
        return PartBytes::Coming;
    }
    // No reader thread (headless): the run wants its bytes in the frame that
    // asked, which is the bargain the whole module strikes there.
    let ready = letter_part(Some(world), mail, at, k.clone());
    let p = cx.global::<Pictures>();
    p.take(&ready);
    match p.bytes.get(&k) {
        Some(b) => PartBytes::Here(b.clone()),
        None => PartBytes::Gone,
    }
}

/// Asks for one letter's pictures, deduplicating requests and allowing failed
/// downloads to retry. The results land in [`landed`].
pub fn want_cid_parts(cx: &mut Cx, world: &World, mid: MailId) {
    let key = parts::image_scope(world.store(), mid);
    let p = cx.global::<Pictures>();
    p.retry_due(&key);
    if !p.asked.insert(key.clone()) {
        return;
    }
    if let Some(tx) = p.reader() {
        let Ok(reader) = world.with_cap::<parts::Reader, _>(|r| r.clone()) else {
            return;
        };
        let _ = tx.send(Job::Cid {
            db: world.store().db(),
            reader,
            mid,
            key,
        });
        return;
    }
    let ready = cid_parts(Some(world), mid, key);
    cx.global::<Pictures>().take(&ready);
}

/// Asks for the bytes a `data:` source carries, once.
fn want_data_bytes(cx: &mut Cx, k: &str, src: &str) {
    let p = cx.global::<Pictures>();
    if !p.asked.insert(k.to_string()) {
        return;
    }
    if let Some(tx) = p.reader() {
        let _ = tx.send(Job::Data {
            key: k.to_string(),
            src: src.to_string(),
        });
        return;
    }
    let ready = data_bytes(k.to_string(), src);
    cx.global::<Pictures>().take(&ready);
}

/// Where the pictures that stood in a link were drawn, taken: the reader
/// asks once its rows have landed, and the next draw fills the list again.
///
/// A picture that is a link is a control, and the panel it sits in owns
/// where the controls of a letter are — that is what the pointer is painted
/// from.
pub fn link_rects(cx: &mut Cx) -> Vec<Rect> {
    std::mem::take(&mut cx.global::<Pictures>().links)
}

/// Files what finished off the frame: bytes the reader thread found, and
/// textures makepad's decode pool finished. True when anything landed, so the
/// panel redraws — a picture that arrives has to be placed, and the item that
/// wants it may be anywhere in the tree.
pub fn landed(cx: &mut Cx, actions: &Actions) -> bool {
    let mut any = false;
    for a in actions {
        if let Some(ready) = a.downcast_ref::<Ready>() {
            cx.global::<Pictures>().take(ready);
            any = true;
        }
        let Some(AsyncImageLoad { image_path, result }) = a.downcast_ref::<AsyncImageLoad>() else {
            continue;
        };
        // Taken here rather than left for the item that asked: the item may be
        // scrolled out of its list by now, and a decode nobody commits leaves
        // the cache entry pending for good. Committing it early costs the item
        // nothing — its own handler reads the texture back out of the cache
        // either way.
        let Some(result) = result.borrow_mut().take() else {
            continue;
        };
        // A picture that would not decode is given up on, so the item that
        // asked stops holding a blank box open for it and shows its alt text.
        if result.is_err() {
            let k = image_path.to_string_lossy().to_string();
            cx.global::<Pictures>().failed.insert(k);
        }
        process_async_image_load(cx, image_path, result);
        any = true;
    }
    any
}

/// Asks the network for `src` unless it is here, on its way, or known not to
/// come. The reply lands in [`arrived`].
fn fetch(cx: &mut Cx, src: &str) {
    let id = LiveId::from_str(src);
    {
        let p = cx.global::<Pictures>();
        if p.bytes.contains_key(src) || p.failed.contains(src) || p.inflight.contains_key(&id) {
            return;
        }
        p.inflight.insert(id, src.to_string());
    }
    cx.http_request(id, HttpRequest::new(src.to_string(), HttpMethod::GET));
}

/// Files the replies to [`fetch`]; true when any image landed or failed, so
/// the panel redraws.
pub fn arrived(cx: &mut Cx, responses: &[NetworkResponse]) -> bool {
    let p = cx.global::<Pictures>();
    let mut any = false;
    for r in responses {
        match r {
            NetworkResponse::HttpResponse {
                request_id,
                response,
            } => {
                let Some(src) = p.inflight.remove(request_id) else {
                    continue;
                };
                match response.get_body() {
                    Some(body)
                        if (200..300).contains(&response.status_code) && !body.is_empty() =>
                    {
                        p.bytes.insert(src, body.clone().into());
                    }
                    _ => {
                        p.failed.insert(src);
                    }
                }
                any = true;
            }
            NetworkResponse::HttpError { request_id, .. } => {
                if let Some(src) = p.inflight.remove(request_id) {
                    p.failed.insert(src);
                    any = true;
                }
            }
            _ => {}
        }
    }
    any
}

/// The largest SVG worth parsing in the frame that draws it. An SVG has no
/// texture and no cache: it becomes geometry on the widget's own script VM,
/// so it cannot leave the UI thread the way a raster decode can. makepad's
/// own ceiling is sixteen megabytes, which is a stalled frame; a picture in a
/// letter is a logo or a diagram, and this is generous for both.
const MAX_INLINE_SVG: usize = 64 << 10;

/// Whether the picture's EXIF says it is stored on its side — orientations 5
/// to 8, the quarter turns, which swap width and height once decoded.
///
/// Read here because the header dimensions makepad reports before a decode
/// are the *encoded* ones, while the buffer it hands back afterwards has the
/// turn applied; the box reserved in between has to agree with the second or
/// it snaps. JPEG only: it is where a rotation tag actually comes from (a
/// photograph off a phone), and PNG and WebP can carry one in theory and
/// essentially never do.
fn exif_turns_the_picture(bytes: &[u8]) -> bool {
    jpeg_exif(bytes)
        .and_then(tiff_orientation)
        .is_some_and(|o| (5..=8).contains(&o))
}

/// The TIFF block of a JPEG's `APP1 Exif` segment, if it has one. Walks the
/// marker chain from `SOI` and stops at the first segment that is not one:
/// EXIF is written before the scan, and reading past it means reading the
/// entropy-coded image.
fn jpeg_exif(bytes: &[u8]) -> Option<&[u8]> {
    let mut at = bytes.strip_prefix(&[0xFF, 0xD8]).map(|_| 2)?;
    loop {
        // Any number of fill bytes may pad the run-up to a marker.
        while bytes.get(at) == Some(&0xFF) && bytes.get(at + 1) == Some(&0xFF) {
            at += 1;
        }
        if bytes.get(at) != Some(&0xFF) {
            return None;
        }
        let marker = *bytes.get(at + 1)?;
        // Start of scan, or anything with no length: past the metadata.
        if marker == 0xDA || marker == 0xD9 || (0xD0..=0xD8).contains(&marker) {
            return None;
        }
        let len = u16::from_be_bytes([*bytes.get(at + 2)?, *bytes.get(at + 3)?]) as usize;
        let body = bytes.get(at + 4..at + 2 + len.max(2))?;
        if marker == 0xE1 {
            if let Some(tiff) = body.strip_prefix(b"Exif\0\0") {
                return Some(tiff);
            }
        }
        at += 2 + len.max(2);
    }
}

/// The Orientation tag (`0x0112`) of a TIFF header block, in either byte
/// order. Only the first IFD is walked — orientation lives there.
fn tiff_orientation(tiff: &[u8]) -> Option<u16> {
    let big = match tiff.get(..2)? {
        b"MM" => true,
        b"II" => false,
        _ => return None,
    };
    let u16_at = |i: usize| -> Option<u16> {
        let b = [*tiff.get(i)?, *tiff.get(i + 1)?];
        Some(if big {
            u16::from_be_bytes(b)
        } else {
            u16::from_le_bytes(b)
        })
    };
    let u32_at = |i: usize| -> Option<u32> {
        let b = [
            *tiff.get(i)?,
            *tiff.get(i + 1)?,
            *tiff.get(i + 2)?,
            *tiff.get(i + 3)?,
        ];
        Some(if big {
            u32::from_be_bytes(b)
        } else {
            u32::from_le_bytes(b)
        })
    };
    if u16_at(2)? != 42 {
        return None;
    }
    let ifd = u32_at(4)? as usize;
    let count = u16_at(ifd)? as usize;
    (0..count).find_map(|i| {
        let e = ifd + 2 + i * 12;
        // A SHORT's value sits in the first two bytes of the value field,
        // whichever end of the four the byte order puts it at.
        (u16_at(e)? == 0x0112).then(|| u16_at(e + 8))?
    })
}

/// How far along one picture is. Nothing here is done in the frame that asks
/// for it: the bytes come from [`Pictures`] and the decode from makepad's
/// pool, so an item goes `Want` → `Loading` → `Shown` across at least two
/// frames, holding its own box open in between.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Pic {
    /// No bytes yet: asked for, or waiting on a source someone else files.
    #[default]
    Want,
    /// Bytes in hand, decoding on the pool. The natural size is known (it
    /// comes off the header), so the box is already the right one.
    Loading,
    /// A texture, or a drawn SVG.
    Shown,
    /// No source to be had, or nothing that decodes: its alt text, for good.
    Failed,
}

/// An `<img>` in a letter: the image item the `Html` widget places in its
/// flow for the tag, sized to its own pixels or its `width` hint and never
/// wider than the column. Its bytes come from [`Pictures`] — a `cid:` part
/// from the file cache or IMAP, a `data:` payload, an HTTP reply, all found
/// off the frame — and the decode from those bytes runs on makepad's pool,
/// keyed in its texture cache so the same picture in two panels is decoded
/// once. Until the bytes come it is its alt text; once they do it holds the
/// box the picture will fill, so nothing reflows when the texture lands. With
/// an `href` — the link the picture sat in — a tap on it is a link click, the
/// same action the text links raise.
#[derive(Script, Widget)]
pub struct HtmlImage {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[layout]
    layout: Layout,
    #[redraw]
    #[live]
    image: Image,
    #[live]
    draw_text: DrawText,
    /// A reading-sized preview. The same width cap keeps adjoining banner
    /// strips aligned; the height cap also fits portrait images.
    #[live(360.0)]
    max_width: f64,
    #[live(320.0)]
    max_height: f64,
    #[rust]
    src: String,
    #[rust]
    alt: String,
    #[rust]
    width: Option<f64>,
    #[rust]
    href: String,
    #[rust]
    state: Pic,
    /// The name its bytes are filed under, and the key its texture sits in
    /// makepad's cache under. Computed once.
    #[rust]
    key: String,
    /// Its own pixels, off the header — known while the decode is still
    /// running, which is what keeps the box from jumping when it lands.
    #[rust]
    nat: Option<(f64, f64)>,
}

impl ScriptHook for HtmlImage {
    fn on_after_new_scoped(&mut self, _vm: &mut ScriptVm, scope: &mut Scope) {
        // The tag's attributes, the way `HtmlLink` reads its href.
        let Some(doc) = scope.props.get::<makepad_html::HtmlDoc>() else {
            return;
        };
        let mut walker = doc.new_walker_with_index(scope.index + 1);
        while let Some((lc, attr)) = walker.while_attr_lc() {
            match lc {
                live_id!(src) => self.src = attr.into(),
                live_id!(alt) => self.alt = attr.into(),
                live_id!(width) => self.width = attr.parse().ok(),
                live_id!(href) => self.href = attr.into(),
                _ => {}
            }
        }
    }
}

impl Widget for HtmlImage {
    /// To the list around it, a picture that is a link is a control — a press
    /// on it taps or drag-scrolls, as on a text link — and any other is part
    /// of the prose: a selection can start on it.
    fn is_interactive(&self) -> bool {
        self.state == Pic::Shown && !self.href.is_empty()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        // An animated texture ticks through the image's own next-frame.
        self.image.handle_event(cx, event, scope);
        if self.href.is_empty() || self.state != Pic::Shown {
            return;
        }
        match event.hits(cx, self.image.area()) {
            Hit::FingerHoverIn(_) => cx.set_cursor(MouseCursor::Hand),
            Hit::FingerHoverOut(_) => cx.set_cursor(MouseCursor::Default),
            Hit::FingerUp(fe) if fe.is_over && fe.is_primary_hit() && fe.was_tap() => {
                cx.widget_action(
                    self.widget_uid(),
                    HtmlLinkAction::Clicked {
                        url: self.href.clone(),
                        key_modifiers: fe.modifiers,
                    },
                );
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, _walk: Walk) -> DrawStep {
        // Asked again while it decodes, not once: re-asking is an early return
        // inside makepad's cache while the job is still on the pool, and the
        // one way back when a *finished* texture is evicted under the cache's
        // cap before this item next drew (which would otherwise hold the box
        // blank for good).
        if matches!(self.state, Pic::Want | Pic::Loading)
            || (self.state == Pic::Failed && self.src.starts_with("cid:"))
        {
            self.load(cx);
        }
        match self.state {
            Pic::Shown => {
                let nat = self
                    .image
                    .size_in_pixels(cx)
                    .map(|(w, h)| (w.max(1) as f64, h.max(1) as f64))
                    .or(self.nat)
                    .unwrap_or((1.0, 1.0));
                let walk = self.box_walk(cx, nat);
                let step = self.image.draw_walk_image(cx, walk);
                // A picture that is a link is a control, so the panel around
                // the letter is told where it landed: it registers the
                // rectangles a letter's controls wear a hand on, and an item
                // can reach nothing itself.
                if !self.href.is_empty() {
                    let r = self.image.area().rect(cx);
                    cx.global::<Pictures>().links.push(r);
                }
                step
            }
            // Bytes in hand, decoding: hold the box the picture will fill
            // rather than lay out the alt text and reflow a frame later.
            Pic::Loading => {
                let walk = self.box_walk(cx, self.nat.unwrap_or((1.0, 1.0)));
                cx.walk_turtle(walk);
                DrawStep::done()
            }
            Pic::Want | Pic::Failed => {
                if !self.alt.is_empty() {
                    self.draw_text
                        .draw_walk(cx, Walk::fit(), Align::default(), &self.alt);
                }
                DrawStep::done()
            }
        }
    }
}

impl HtmlImage {
    /// Fit the whole picture within the reader's preview box and column.
    /// Smaller pictures keep their size, and every scale preserves aspect.
    fn box_walk(&self, cx: &Cx2d, (nw, nh): (f64, f64)) -> Walk {
        let mut w = self.width.filter(|w| *w >= 1.0).unwrap_or(nw).min(nw);
        let avail = cx.turtle().inner_width();
        if avail.is_finite() && avail > 1.0 {
            w = w.min(avail);
        }
        w = w.min(self.max_width).min(self.max_height * nw / nh);
        Walk {
            width: Size::Fixed(w),
            height: Size::Fixed(w * nh / nw),
            ..Walk::default()
        }
    }

    /// Asks for the bytes and starts the decode, once they are here. A source
    /// that cannot be had or read is given up on rather than asked every
    /// frame; one whose bytes are still coming is simply asked again next
    /// frame, since asking is one lookup.
    fn load(&mut self, cx: &mut Cx2d) {
        if self.key.is_empty() {
            self.key = key(&self.src);
        }
        // Where the bytes come from, if nobody has filed them yet. A `cid:`
        // part is the letter's own and its panel asks for it (see
        // [`want_cid_parts`]); the other two are this item's to ask for.
        if self.src.starts_with("data:") {
            want_data_bytes(cx, &self.key, &self.src);
        } else if self.src.starts_with("http") {
            fetch(cx, &self.src);
        }
        let p = cx.global::<Pictures>();
        if p.failed.contains(&self.key) {
            self.state = Pic::Failed;
            return;
        }
        let Some(bytes) = p.bytes.get(&self.key).cloned() else {
            return;
        };
        // An SVG is drawn rather than decoded: it becomes geometry on the
        // widget's own VM, which no thread can be handed, so the parse can
        // only happen here. Hence the cap — makepad would take sixteen
        // megabytes of it, and a document that size is a stalled frame by any
        // other name. A picture in a letter is a logo or a diagram; one past
        // this is alt text, said once.
        if looks_like_svg(&bytes) {
            if bytes.len() > MAX_INLINE_SVG {
                self.fail(cx);
                return;
            }
            match self.image.load_svg_from_shared_data(cx, bytes) {
                Ok(()) => self.state = Pic::Shown,
                Err(_) => self.fail(cx),
            }
            return;
        }
        let path = PathBuf::from(&self.key);
        // The decode and its mip chain go to makepad's pool, keyed in its
        // texture cache; `Loading` carries the size off the header, which is
        // what lets the box be right before the pixels are. `Loaded` is a
        // decode that already happened — another item's, or this frame's,
        // since makepad decodes inline under `MAKEPAD=headless` — and the
        // texture is on the widget by the time it says so.
        match ImageCacheImpl::load_image_from_data_async_impl(
            &mut self.image,
            cx,
            &path,
            bytes.clone(),
            0,
        ) {
            Ok(AsyncLoadResult::Loaded) => self.state = Pic::Shown,
            Ok(AsyncLoadResult::Loading(w, h)) => {
                let (w, h) = (w.max(1) as f64, h.max(1) as f64);
                // The header's width and height are the *encoded* ones; a
                // quarter-turn of EXIF orientation is applied by the decoder
                // and not by the header, so the box has to turn with it or a
                // portrait photograph reserves a landscape hole and snaps
                // when the texture lands.
                self.nat = Some(if exif_turns_the_picture(&bytes) {
                    (h, w)
                } else {
                    (w, h)
                });
                self.state = Pic::Loading;
            }
            Err(_) => self.fail(cx),
        }
    }

    /// Gives up on this source — here and for every other item that names it,
    /// so one letter's broken picture is decoded no more than once.
    fn fail(&mut self, cx: &mut Cx2d) {
        self.state = Pic::Failed;
        cx.global::<Pictures>().failed.insert(self.key.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A JPEG with an `APP1 Exif` segment over a one-entry TIFF block, in
    /// either byte order.
    fn jpeg_oriented(orientation: u16, big: bool) -> Vec<u8> {
        let u16b = |v: u16| {
            if big {
                v.to_be_bytes()
            } else {
                v.to_le_bytes()
            }
        };
        let u32b = |v: u32| {
            if big {
                v.to_be_bytes()
            } else {
                v.to_le_bytes()
            }
        };
        let mut tiff: Vec<u8> = if big { b"MM".to_vec() } else { b"II".to_vec() };
        tiff.extend(u16b(42));
        tiff.extend(u32b(8));
        tiff.extend(u16b(1)); // one entry
        tiff.extend(u16b(0x0112)); // Orientation
        tiff.extend(u16b(3)); // SHORT
        tiff.extend(u32b(1)); // count
        tiff.extend(u16b(orientation));
        tiff.extend(u16b(0)); // the other half of the value field
        tiff.extend(u32b(0)); // next IFD

        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend(tiff);
        let mut out = vec![0xFF, 0xD8, 0xFF, 0xE1];
        out.extend(((app1.len() + 2) as u16).to_be_bytes());
        out.extend(app1);
        out.extend([0xFF, 0xDA, 0x00, 0x02]);
        out
    }

    /// A quarter turn swaps the box before the decode does; the upright
    /// orientations leave it alone.
    #[test]
    fn a_quarter_turn_of_exif_is_read_before_the_decode() {
        for o in 5..=8 {
            assert!(
                exif_turns_the_picture(&jpeg_oriented(o, false)),
                "little-endian {o}"
            );
            assert!(
                exif_turns_the_picture(&jpeg_oriented(o, true)),
                "big-endian {o}"
            );
        }
        for o in [1, 2, 3, 4] {
            assert!(
                !exif_turns_the_picture(&jpeg_oriented(o, false)),
                "upright {o}"
            );
        }
    }

    /// Anything that is not a JPEG with EXIF keeps its axes, including a
    /// truncated one — the walk stops at the scan and never reads past it.
    #[test]
    fn a_picture_with_no_exif_keeps_its_axes() {
        assert!(!exif_turns_the_picture(&[0xFF, 0xD8, 0xFF, 0xDA, 0, 2]));
        assert!(!exif_turns_the_picture(b"\x89PNG\r\n\x1a\n"));
        assert!(!exif_turns_the_picture(&jpeg_oriented(6, false)[..8]));
        assert!(!exif_turns_the_picture(&[]));
    }

    /// A `data:` source is filed under sixteen hex characters, not under
    /// megabytes of its own payload; anything else is filed under itself.
    #[test]
    fn a_data_source_is_named_by_its_hash() {
        let src = format!("data:image/png;base64,{}", "A".repeat(4096));
        let named = key(&src);
        assert!(named.starts_with("data:") && named.len() == 21, "{named}");
        assert_eq!(key("https://x.dev/a.png"), "https://x.dev/a.png");
        assert_eq!(part_key("m7", 3), "part:m7/3");
    }

    /// The bytes a `data:` source carries come back un-base64'd; one that is
    /// not base64 is an answer, not a wait.
    #[test]
    fn a_data_source_is_un_base64d_off_the_frame() {
        let ready = data_bytes("k".into(), "data:image/png;base64,aGVsbG8=");
        assert_eq!(ready.items.len(), 1);
        assert_eq!(&*ready.items[0].1, b"hello");
        let bad = data_bytes("k".into(), "data:image/png;base64,not base64!");
        assert!(bad.items.is_empty() && bad.failed == vec!["k".to_string()]);
    }
}
