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
    sync::sync_account(s.world(), seed::ACCOUNT).unwrap();
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
    assert_eq!(parts::part(s.world(), &a).unwrap(), bytes);
    assert_eq!(srv.with(seed::ACCOUNT, |s| s.part_fetches.len()), Some(1));
    srv.set_down(Some("offline"));
    assert_eq!(parts::part(s.world(), &a).unwrap(), bytes);
    assert_eq!(srv.with(seed::ACCOUNT, |s| s.part_fetches.len()), Some(1));
    assert_eq!(
        model::raw(s.store(), mail).unwrap(),
        raw,
        "downloads never write payloads to the store"
    );
    clear_cache(&s);
    assert!(parts::part(s.world(), &a).unwrap_err().contains("offline"));
    srv.set_down(None);
    assert_eq!(parts::part(s.world(), &a).unwrap(), bytes);
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
    let error = parts::part(s.world(), &a).unwrap_err();
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
    assert_eq!(parts::part(s.world(), &a).unwrap(), b"from inbox");
    assert_eq!(
        servers(&s)
            .with(seed::ACCOUNT, |s| s.part_fetches[0].0.clone())
            .unwrap(),
        "INBOX"
    );
}

#[test]
fn old_raw_messages_are_converted_and_their_parts_remain_addressable() {
    let (s, _) = session();
    let m = seed::mails()
        .into_iter()
        .find(|m| !seed::parts_of(&m.subject).is_empty())
        .unwrap();
    let subject = m.subject.clone();
    let mail: i64 = s
        .store()
        .conn()
        .query_row(
            "SELECT id FROM message WHERE subject = ?1",
            [&subject],
            |r| r.get(0),
        )
        .unwrap();
    let before = parts::attachments(s.store(), mail).as_ref().clone();
    let raw = seed::rfc822(&m);
    s.store()
        .write(move |c| {
            c.execute(
                "UPDATE message SET raw = ?2 WHERE id = ?1",
                rusqlite::params![mail, raw.as_bytes()],
            )?;
            c.execute(
                "UPDATE attachment_scan SET version = 1 WHERE message = ?1",
                [mail],
            )?;
            parts::scan(c)
        })
        .unwrap();
    assert_eq!(*parts::attachments(s.store(), mail), before);
    let compact = model::raw(s.store(), mail).unwrap();
    assert!(compact.starts_with(b"superapp-mail-1\n"));
    assert_eq!(
        parts::part(s.world(), &before[0]).unwrap(),
        seed::parts_of(&subject)[0].1
    );
    s.store().write(|c| parts::scan(c)).unwrap();
    assert_eq!(
        model::raw(s.store(), mail).unwrap(),
        compact,
        "conversion is idempotent"
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
    assert!(!sync::sync_account(s.world(), seed::ACCOUNT).unwrap());
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
    assert!(!sync::sync_account(s.world(), seed::ACCOUNT).unwrap());
    assert_eq!(count("repaired mail"), 1);
    assert_eq!(count("older good mail"), 1);
}

#[test]
fn unreadable_legacy_data_does_not_block_conversion_or_repeat_forever() {
    let (s, _) = session();
    let invalid = vec![
        b"".to_vec(),
        b"superapp-mail-1\n{broken json".to_vec(),
    ];
    for raw in &invalid {
        assert!(content::Content::read(raw).is_err(), "{raw:?}");
    }
    let m = seed::mails()
        .into_iter()
        .find(|m| !seed::parts_of(&m.subject).is_empty())
        .unwrap();
    let valid = seed::rfc822(&m);
    let expected = content::compact(valid.as_bytes()).unwrap();
    let bad_raw = invalid.clone();
    let (bad, good) = s
        .store()
        .write(move |c| {
            let folder: i64 =
                c.query_row("SELECT id FROM folder WHERE name = 'INBOX'", [], |r| {
                    r.get(0)
                })?;
            let attachments = sync::parse_mail(valid.as_bytes()).attachments;
            let mut bad = Vec::new();
            for raw in bad_raw {
                c.execute(
                    "INSERT INTO message(account, folder, date, raw) VALUES(?1, ?2, 0, ?3)",
                    rusqlite::params![seed::ACCOUNT, folder, raw],
                )?;
                let id = c.last_insert_rowid();
                parts::attach_tx(c, id, &attachments)?;
                c.execute(
                    "UPDATE attachment_scan SET version = 1 WHERE message = ?1",
                    [id],
                )?;
                bad.push(id);
            }
            c.execute(
                "INSERT INTO message(account, folder, date, raw) VALUES(?1, ?2, 0, ?3)",
                rusqlite::params![seed::ACCOUNT, folder, valid.as_bytes()],
            )?;
            let good = c.last_insert_rowid();
            parts::scan(c)?;
            Ok((bad, good))
        })
        .unwrap();
    assert_eq!(model::raw(s.store(), good).unwrap(), expected);
    assert!(!parts::attachments(s.store(), good).is_empty());
    for (id, raw) in bad.iter().zip(&invalid) {
        assert_eq!(
            model::raw(s.store(), *id).as_ref(),
            Some(raw),
            "keep the original for recovery"
        );
        assert!(parts::attachments(s.store(), *id).is_empty());
    }
    let pending: i64 = s.store().conn().query_row(
        "SELECT COUNT(*) FROM message m LEFT JOIN attachment_scan s ON s.message = m.id WHERE s.version IS NULL OR s.version != ?1",
        [parts::ATTACH_VERSION], |r| r.get(0),
    ).unwrap();
    assert_eq!(
        pending, 0,
        "unreadable messages must not keep the sender busy"
    );
    s.store().write(|c| parts::scan(c)).unwrap();
    assert_eq!(model::raw(s.store(), good).unwrap(), expected);
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
