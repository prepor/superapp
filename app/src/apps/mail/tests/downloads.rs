//! Mailbox sync carries readings and descriptors. Only a file request
//! downloads bytes, and subsequent requests can use the local cache offline.

use super::*;
use crate::apps::mail::{content, parts};
use base64::Engine as _;
use kernel::caps::Blobs;

fn deliver(s: &Session, subject: &str, bytes: &[u8]) -> MailId {
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    let raw = format!("From: sender@example.org\r\nTo: {}\r\nSubject: {subject}\r\nMessage-ID: <{subject}@example.org>\r\nContent-Type: multipart/mixed; boundary=x\r\n\r\n\
--x\r\nContent-Type: text/plain\r\n\r\nreadable offline\r\n\
--x\r\nContent-Type: application/octet-stream\r\nContent-Disposition: attachment; filename=download.bin\r\nContent-Transfer-Encoding: base64\r\n\r\n{encoded}\r\n--x--\r\n", seed::ADDRESS);
    servers(s).with(seed::ACCOUNT, |srv| {
        srv.deliver_flagged("INBOX", true, false, &raw);
    });
    kernel::runtime::block_on(sync::sync_account(s.world(), seed::ACCOUNT)).unwrap();
    s.store()
        .conn()
        .query_row(
            "SELECT id FROM message WHERE subject = ?1",
            [subject],
            |r| r.get(0),
        )
        .unwrap()
}

fn clear_cache(s: &Session) {
    // Replacing the handle emulates an empty/evicted device cache.
    s.world()
        .caps(|c| c.insert::<dyn Blobs>(Box::new(Env::default().blobs)));
}

fn agent_read(s: &Session, mail: MailId, part: u32) -> Result<serde_json::Value, String> {
    let tool = s.apps().tool("mail.attachment").unwrap();
    assert!(!tool.writes && !tool.asks);
    let input = serde_json::json!({"mail": mail, "part": part});
    tool.check(&input)?;
    kernel::runtime::block_on((tool.reader.unwrap())(&input)(s.world()))
}

#[test]
fn an_agent_discovers_and_reads_pdf_attachments_with_no_panel_or_manual_export() {
    let (s, _) = session();
    let bytes = crate::reader::document::test_pdf("Bonjour depuis le PDF.");
    let mail = deliver(&s, "agent-pdf", &bytes);
    let tool = s.apps().tool("mail.thread").unwrap().clone();
    let input = serde_json::json!({"thread": mail});
    let thread = kernel::runtime::block_on(tool.reader.unwrap()(&input)(s.world())).unwrap();
    let part = &thread["letters"][0]["attachments"][0];
    assert_eq!(part["mail"], mail);
    assert_eq!(part["name"], "download.bin");
    let at = part["part"].as_u64().unwrap() as u32;
    assert_eq!(servers(&s).with(seed::ACCOUNT, |srv| srv.part_fetches.len()), Some(0));
    let result = agent_read(&s, mail, at).unwrap();
    assert!(result["text"].as_str().unwrap().contains("Bonjour depuis le PDF."));
    assert_eq!(result["format"], "pdf");
    assert_eq!(result["size"], bytes.len());
    servers(&s).set_down(Some("offline"));
    assert_eq!(agent_read(&s, mail, at).unwrap(), result);
    assert_eq!(servers(&s).with(seed::ACCOUNT, |srv| srv.part_fetches.len()), Some(1));
    assert!(model::mail(s.store(), mail).unwrap().head.unread);
    clear_cache(&s);
    assert!(agent_read(&s, mail, at).unwrap_err().contains("offline"));
    assert!(agent_read(&s, mail, u32::MAX).unwrap_err().contains("no attachment"));
}

#[test]
fn attachments_download_only_on_demand_and_cache_hits_work_offline() {
    let (s, _) = session();
    let bytes = vec![0xa7; 1024 * 1024];
    let mail = deliver(&s, "large-file", &bytes);
    let a = parts::attachments(s.store(), mail)[0].clone();
    assert_eq!(a.size, bytes.len() as u64);
    let raw = model::raw(s.store(), mail).unwrap();
    assert!(
        raw.len() < 4096,
        "one megabyte file must not be in SQLite: {}",
        raw.len()
    );
    assert!(content::Content::read(&raw).unwrap().reading.len() < 1024);
    assert_eq!(
        model::mail(s.store(), mail).unwrap().body,
        "readable offline"
    );
    let srv = servers(&s);
    assert!(srv
        .with(seed::ACCOUNT, |s| s.part_fetches.is_empty())
        .unwrap());
    assert_eq!(kernel::runtime::block_on(parts::part(s.world(), &a)).unwrap(), bytes);
    assert_eq!(srv.with(seed::ACCOUNT, |s| s.part_fetches.len()), Some(1));
    srv.set_down(Some("offline"));
    assert_eq!(kernel::runtime::block_on(parts::part(s.world(), &a)).unwrap(), bytes);
    assert_eq!(srv.with(seed::ACCOUNT, |s| s.part_fetches.len()), Some(1));
    assert_eq!(
        model::raw(s.store(), mail).unwrap(),
        raw,
        "downloads never write payloads to the store"
    );
    clear_cache(&s);
    assert!(kernel::runtime::block_on(parts::part(s.world(), &a)).unwrap_err().contains("offline"));
    srv.set_down(None);
    assert_eq!(kernel::runtime::block_on(parts::part(s.world(), &a)).unwrap(), bytes);
    assert_eq!(srv.with(seed::ACCOUNT, |s| s.part_fetches.len()), Some(2));
    assert!(model::mail(s.store(), mail).unwrap().head.unread);
    assert!(srv
        .with(seed::ACCOUNT, |s| s.folders["INBOX"]
            .2
            .last()
            .unwrap()
            .unread)
        .unwrap());
}

#[test]
fn a_changed_uid_generation_refuses_to_download_from_the_old_location() {
    let (s, _) = session();
    let mail = deliver(&s, "uid-reuse", b"the original file");
    let a = parts::attachments(s.store(), mail)[0].clone();
    servers(&s).with(seed::ACCOUNT, |srv| {
        srv.folders.get_mut("INBOX").unwrap().0 += 1
    });
    let error = kernel::runtime::block_on(parts::part(s.world(), &a)).unwrap_err();
    assert!(error.contains("mailbox changed"), "{error}");
    assert!(servers(&s)
        .with(seed::ACCOUNT, |s| s.part_fetches.is_empty())
        .unwrap());
}

#[test]
fn a_pending_move_downloads_from_the_servers_folder() {
    let (s, _) = session();
    let mail = deliver(&s, "pending-move", b"from inbox");
    let archive: i64 = s
        .store()
        .conn()
        .query_row("SELECT id FROM folder WHERE role = 'archive'", [], |r| {
            r.get(0)
        })
        .unwrap();
    s.store()
        .write(move |c| {
            c.execute(
                "UPDATE message SET folder = ?2 WHERE id = ?1",
                rusqlite::params![mail, archive],
            )?;
            Ok(())
        })
        .unwrap();
    let a = parts::attachments(s.store(), mail)[0].clone();
    assert_eq!(kernel::runtime::block_on(parts::part(s.world(), &a)).unwrap(), b"from inbox");
    assert_eq!(
        servers(&s)
            .with(seed::ACCOUNT, |s| s.part_fetches[0].0.clone())
            .unwrap(),
        "INBOX"
    );
}

#[test]
fn an_unusable_batch_does_not_hide_older_mail_and_is_retried_later() {
    let (s, _) = session();
    let srv = servers(&s);
    srv.with(seed::ACCOUNT, |srv| {
        srv.folder("INBOX", 9);
        srv.backfills.clear();
        srv.deliver_flagged(
            "INBOX",
            true,
            false,
            "Subject: older good mail\r\n\r\nhello",
        );
        for _ in 0..sync::BACKFILL {
            srv.deliver_flagged("INBOX", true, false, "");
        }
        srv.folder("Trash", 9);
        srv.deliver_flagged("Trash", true, false, "Subject: later folder\r\n\r\nhello");
    })
    .unwrap();
    assert!(!kernel::runtime::block_on(sync::sync_account(s.world(), seed::ACCOUNT)).unwrap());
    let count = |subject: &str| -> i64 {
        s.store()
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM message WHERE subject = ?1",
                [subject],
                |r| r.get(0),
            )
            .unwrap()
    };
    assert_eq!(count("older good mail"), 1);
    assert_eq!(count("later folder"), 1);
    let batches = srv
        .with(seed::ACCOUNT, |srv| srv.backfills.clone())
        .unwrap();
    let inbox: Vec<_> = batches.iter().filter(|(name, _)| name == "INBOX").collect();
    assert_eq!(inbox.len(), 2);
    assert_eq!(inbox[0].1.len(), sync::BACKFILL);
    assert_eq!(inbox[1].1, [1]);

    // A repaired server response becomes available on the next pass; no
    // permanent skip marker loses the message or suppresses its retry.
    srv.with(seed::ACCOUNT, |srv| {
        srv.folders.get_mut("INBOX").unwrap().2[1].raw =
            b"Subject: repaired mail\r\n\r\nhello".to_vec();
    });
    assert!(!kernel::runtime::block_on(sync::sync_account(s.world(), seed::ACCOUNT)).unwrap());
    assert_eq!(count("repaired mail"), 1);
    assert_eq!(count("older good mail"), 1);
}

#[test]
fn image_sources_are_traced_and_refresh_when_the_server_identity_changes() {
    let (s, _) = session();
    let mail = deliver(&s, "cached-image-source", b"image");
    let store = s.store();
    store.trace_begin(73);
    let mut scope = parts::image_scope(store, mail);
    assert_eq!(parts::image_scope(store, mail), scope);
    store.trace_end();
    let trace = store.trace_of(73);
    assert_eq!(
        trace.len(),
        1,
        "image identity reads participate in the panel's query trace"
    );
    assert_eq!(trace[0].id, "mail image source");
    // Reading the same database from a worker uses the same namespace;
    // another store with matching row ids must remain separate.
    let reader = Store::with_db(store.db()).unwrap();
    assert_eq!(parts::image_scope(&reader, mail), scope);
    let (other, _) = session();
    assert_ne!(
        parts::image_scope(store, 1),
        parts::image_scope(other.store(), 1)
    );

    for sql in [
        "UPDATE account SET imap_host = 'changed.example.org' WHERE id = (SELECT account FROM message WHERE id = ?1)",
        "UPDATE folder SET name = name || '-renamed' WHERE id = (SELECT folder FROM server_msg WHERE message = ?1)",
        "UPDATE folder SET uidvalidity = uidvalidity + 1 WHERE id = (SELECT folder FROM server_msg WHERE message = ?1)",
        "UPDATE server_msg SET uid = uid + 1000 WHERE message = ?1",
    ] {
        store.write(move |c| c.execute(sql, [mail]).map(|_| ())).unwrap();
        let next = parts::image_scope(store, mail);
        assert_ne!(next, scope, "{sql}");
        scope = next;
    }
    // The desired folder can move before the server does. Its change must
    // invalidate the query without changing which source is downloaded.
    store.write(move |c| c.execute("UPDATE message SET folder = (SELECT id FROM folder WHERE role = 'archive') WHERE id = ?1", [mail]).map(|_| ())).unwrap();
    assert_eq!(parts::image_scope(store, mail), scope);
}
