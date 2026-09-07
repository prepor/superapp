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
