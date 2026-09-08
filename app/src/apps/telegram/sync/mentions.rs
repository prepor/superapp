//! Unread replies and mentions use TDLib's filtered search, independently
//! of how much ordinary history has been downloaded or marked read.

use super::*;
use std::collections::BTreeSet;

pub(super) struct Scan {
    generation: u64,
    known: BTreeSet<MsgId>,
    found: BTreeSet<MsgId>,
}

impl<T: Td> Account<T> {
    /// Replace a scan when Telegram's count changes. A page from an older
    /// generation must not restore notifications read on another device.
    pub(super) fn want_mentions(&self, w: &World, chat: PeerId) {
        self.pages
            .borrow_mut()
            .retain(|p| p.chat != chat || !matches!(p.walk, Walk::Mentions(_)));
        self.mention_scans.borrow_mut().remove(&chat);
        let needed = model::reply_chats(w.store())
            .iter()
            .any(|(peer, _)| *peer == chat);
        runtime::of(w.store()).set_mentions_status(chat, needed, false);
        if needed {
            let generation = self.mention_generation.get() + 1;
            self.mention_generation.set(generation);
            let known = super::super::project::unread_mention_ids(w.store().conn(), chat);
            let Ok(known) = known else {
                self.filed(w, "unread mentions snapshot", known);
                runtime::of(w.store()).set_mentions_status(chat, false, true);
                return;
            };
            self.mention_scans.borrow_mut().insert(
                chat,
                Scan {
                    generation,
                    known: known.into_iter().collect(),
                    found: BTreeSet::new(),
                },
            );
            self.pages.borrow_mut().push_front(Page {
                chat,
                topic: 0,
                from: 0,
                walk: Walk::Mentions(generation),
                view: None,
            });
        }
    }

    pub(super) fn accept_page(&self, page: Page) -> bool {
        // A delayed response cannot release a newer request's pacing slot.
        let active = self
            .in_flight
            .get()
            .is_some_and(|(active, _)| active == page);
        if active {
            self.in_flight.set(None);
        }
        match page.walk {
            Walk::Mentions(generation) => {
                active
                    && self
                        .mention_scans
                        .borrow()
                        .get(&page.chat)
                        .is_some_and(|s| s.generation == generation)
            }
            _ => true,
        }
    }

    pub(super) fn finish_page(&self, w: &World, page: Page, failed: bool) {
        if let Walk::Mentions(generation) = page.walk {
            if self
                .mention_scans
                .borrow()
                .get(&page.chat)
                .is_some_and(|s| s.generation == generation)
            {
                let scan = self.mention_scans.borrow_mut().remove(&page.chat).unwrap();
                let mut failed = failed;
                if !failed {
                    // Only reconcile notifications present when the scan
                    // began; newly arriving messages are not in its snapshot.
                    let gone: Vec<_> = scan.known.difference(&scan.found).copied().collect();
                    let result = w.store().write(move |c| {
                        super::super::project::dismiss_mentions(c, page.chat, &gone)
                    });
                    failed = result.is_err();
                    self.filed(w, "reconcile unread mentions", result);
                }
                runtime::of(w.store()).set_mentions_status(page.chat, false, failed);
            }
        } else {
            runtime::of(w.store()).set_loading_in(page.chat, page.topic, false);
        }
    }

    pub(super) fn on_mentions(&self, w: &World, v: &Value, page: Page) {
        let chat = page.chat;
        let batch: Vec<IncomingMessage> = v["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(updates::message)
            .filter(|m| m.chat == chat)
            .collect();
        let oldest = batch.iter().map(|m| m.id).min();
        if let Some(scan) = self.mention_scans.borrow_mut().get_mut(&chat) {
            scan.found
                .extend(batch.iter().filter(|m| m.unread_mention).map(|m| m.id));
        }
        // Current TDLib supplies a continuation; older releases return
        // `messages`, where the next request starts just below the last id.
        // Short pages are not the end: TDLib may return fewer than requested.
        let next = v["next_from_message_id"]
            .as_i64()
            .or_else(|| oldest.map(|id| id - 1));
        let written = w.store().write(move |c| {
            ensure_peer(c, chat)?;
            model::ensure_chat_tx(c, chat)?;
            for m in &batch {
                if let Some(sender) = m.sender {
                    ensure_peer(c, sender)?;
                }
            }
            project_messages(c, &batch)
        });
        let failed = written.is_err();
        self.filed(w, "unread mentions", written);
        if !failed && oldest.is_some() {
            if let Some(from) = next.filter(|id| *id > 0 && (page.from == 0 || *id < page.from)) {
                self.pages.borrow_mut().push_back(Page { from, ..page });
                return;
            }
        }
        self.finish_page(w, page, failed);
    }

    pub(super) fn on_mention_read(&self, w: &World, u: &Value) {
        let (Some(chat), Some(id), Some(count)) = (
            u["chat_id"].as_i64(),
            u["message_id"].as_i64(),
            u["unread_mention_count"].as_i64(),
        ) else {
            return;
        };
        self.filed(
            w,
            "mention read",
            w.store().write(move |c| {
                super::super::project::read_mentions(c, chat, &[id])?;
                super::super::project::set_mentions(c, chat, count)
            }),
        );
        self.want_mentions(w, chat);
    }
}
