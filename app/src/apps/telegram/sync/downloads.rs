//! A download is one acknowledged operation from source refresh through cache
//! arrival, and through disk copy when the person explicitly saves it.
//! Its retry always starts at the source; no session-local file id is replayed.

use kernel::caps::Blobs;
use kernel::effect::World;
use serde_json::Value;

use super::super::{downloads, model, requests, runtime, updates};
use super::{Account, Td};

pub(super) struct Download {
    file: i64,
    reference: String,
    name: String,
    cache_only: bool,
}

fn source(context: &str) -> Option<(i64, i64, bool)> {
    requests::parse_save_extra(context).map(|(chat, msg)| (chat, msg, false))
        .or_else(|| requests::parse_cache_extra(context).map(|(chat, msg)| (chat, msg, true)))
}

impl<T: Td> Account<T> {
    /// The worker can export cached documents without contacting Telegram,
    /// including during startup. All filesystem work stays off the UI thread.
    pub(super) fn cached_download(&self, w: &World, request: &Value) -> bool {
        if request["@type"] != "getMessage" {
            return false;
        }
        let Some((chat, msg, cache_only)) = request["@extra"]["context"]
            .as_str()
            .and_then(source)
        else {
            return false;
        };
        let Some(id) = request["@extra"]["operation"].as_u64() else {
            return false;
        };
        if !runtime::of(w.store())
            .operations
            .saving_attempt(&request["@extra"])
        {
            return true;
        }
        self.downloads.borrow_mut().remove(&id);
        let Some(m) = model::line(w.store(), chat, msg) else {
            return false;
        };
        // Only documents retain their original filename in the projection.
        // Other media refreshes its metadata before saving, even when cached.
        if !cache_only && m.media.as_ref().is_none_or(|md| md.kind != "file") {
            return false;
        }
        let Some(reference) = downloads::reference(&m) else {
            return false;
        };
        self.save_cached(w, id, reference, &downloads::name(&m), cache_only)
    }

    fn save_cached(&self, w: &World, id: u64, reference: &str, name: &str, cache_only: bool) -> bool {
        let rt = runtime::of(w.store());
        let path = match w.with_cap::<dyn Blobs, _>(|b| b.get(reference)) {
            Ok(Some(path)) => path,
            Ok(None) => return false,
            Err(error) => {
                rt.operations.fail(w.store(), id, &error, false);
                return true;
            }
        };
        if cache_only {
            rt.operations.cached(id);
            return true;
        }
        match downloads::save(w, &path, name) {
            Ok(path) => rt.operations.saved(id, &path),
            Err(error) => rt.operations.fail(w.store(), id, &error, false),
        }
        true
    }

    pub(super) fn on_download_reply(&self, w: &World, v: &Value) -> bool {
        let Some(context) = v["@extra"]["context"].as_str() else {
            return false;
        };
        let Some((chat, msg, cache_only)) = source(context) else {
            return false;
        };
        let Some(id) = v["@extra"]["operation"].as_u64() else {
            return false;
        };
        let rt = runtime::of(w.store());
        if !rt.operations.saving_attempt(&v["@extra"]) {
            return true;
        }
        match v["@type"].as_str() {
            Some("error") => {
                self.downloads.borrow_mut().remove(&id);
                rt.operations.reply(w.store(), v);
            }
            Some("message") => {
                if self.downloads.borrow().contains_key(&id) {
                    return true;
                }
                if v["chat_id"] != chat || v["id"] != msg {
                    rt.operations.fail(
                        w.store(),
                        id,
                        "Telegram returned a different message",
                        false,
                    );
                    return true;
                }
                self.on_new_message(w, v);
                let Some(file) = updates::attachment_file(&v["content"]) else {
                    rt.operations.fail(
                        w.store(),
                        id,
                        "This message no longer has a downloadable file",
                        false,
                    );
                    return true;
                };
                let Some(reference) = updates::file_ref(file) else {
                    rt.operations.fail(
                        w.store(),
                        id,
                        "This file is not available on Telegram yet",
                        false,
                    );
                    return true;
                };
                if cache_only {
                    let size = file["size"].as_u64().unwrap_or(0)
                        .max(file["expected_size"].as_u64().unwrap_or(0));
                    if let Err(error) = crate::reader::document::check_size(size) {
                        rt.operations.fail(w.store(), id, &error, false);
                        return true;
                    }
                }
                let name = updates::attachment_name(&v["content"])
                    .map(downloads::safe_name)
                    .or_else(|| model::line(w.store(), chat, msg).map(|m| downloads::name(&m)))
                    .unwrap_or_else(|| "telegram-file".into());
                self.on_file(w, file);
                if self.save_cached(w, id, &reference, &name, cache_only) {
                    return true;
                }
                let file_id = file["id"].as_i64().unwrap();
                rt.operations.saving_file(id, file);
                self.downloads.borrow_mut().insert(
                    id,
                    Download {
                        file: file_id,
                        reference,
                        name,
                        cache_only,
                    },
                );
                let mut request: Value =
                    serde_json::from_str(&requests::download_media(file_id as i32, context))
                        .unwrap();
                request["@extra"] = v["@extra"].clone();
                self.send(w, &request.to_string());
            }
            Some("file") => {
                let mut pending = self.downloads.borrow_mut();
                let Some(download) = pending.get(&id) else {
                    return true;
                };
                if v["id"] != download.file {
                    return true;
                }
                self.on_file(w, v);
                if v["local"]["is_downloading_completed"] != true {
                    return true;
                }
                let download = pending.remove(&id).unwrap();
                drop(pending);
                if !self.save_cached(w, id, &download.reference, &download.name, download.cache_only) {
                    rt.operations.fail(
                        w.store(),
                        id,
                        "Downloaded file could not be read from the media cache",
                        false,
                    );
                }
            }
            _ => rt.operations.fail(
                w.store(),
                id,
                "Telegram returned an unexpected download response",
                false,
            ),
        }
        true
    }
}
