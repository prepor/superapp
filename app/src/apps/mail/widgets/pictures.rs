//! Mail's inline files, loaded into the shared reader's image cache.

use super::super::{model::MailId, parts};
pub use crate::reader::pictures::{arrived, landed, link_rects};
use crate::reader::pictures::{part_key, Held, Job, Pictures, Ready};
use kernel::effect::World;
use kernel::store::Db;
use makepad_widgets::*;
use std::sync::Arc;

fn hold(held: &mut Held, db: Arc<Db>, reader: parts::Reader) -> Option<&World> {
    if !held.as_ref().is_some_and(|(h, _)| Arc::ptr_eq(h, &db)) {
        *held = reader.world(db.clone()).ok().map(|w| (db, w));
    }
    held.as_ref().map(|(_, w)| w)
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
        let db = world.store().db();
        let _ = tx.send(Job::Read(Box::new(move |held| {
            letter_part(hold(held, db, reader), mail, at, k)
        })));
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
        let db = world.store().db();
        let _ = tx.send(Job::Read(Box::new(move |held| {
            cid_parts(hold(held, db, reader), mid, key)
        })));
        return;
    }
    let ready = cid_parts(Some(world), mid, key);
    cx.global::<Pictures>().take(&ready);
}
