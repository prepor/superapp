//! Device-sync test helper: acting as the lease holder, append a marker to
//! the single compose draft and publish it — a peer's edit that a *running*
//! follower must pick up live, since a compose widget seeds its fields from
//! the row once and then keeps its own buffers.
//!
//! `reseed-edit <db> <bucket-url>`
//!
//! It names no app in code: the row it touches is named on the command line
//! in SQL, so this binary stays what it is — a peer that writes and
//! publishes.

use std::path::Path;

use kernel::repl::object::HttpBucket;
use kernel::repl::{self, Role};
use kernel::store::Store;

/// The column and table a peer's edit lands in. Given here rather than
/// imported: this is a test peer, not part of the build's app graph.
const READ: &str = "SELECT panel FROM draft ORDER BY panel LIMIT 1";
const EDIT: &str = "UPDATE draft SET body = body || ' beta' WHERE panel = ?1";
const SHOW: &str = "SELECT body FROM draft WHERE panel = ?1";

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (Some(db), Some(url)) = (args.get(1), args.get(2)) else {
        eprintln!("usage: reseed-edit <db> <bucket-url>");
        std::process::exit(2);
    };
    let store = Store::open(Some(Path::new(db)), &[]).expect("open store");
    let bucket = HttpBucket::new(url);

    // Re-adopt the lease (the app that seeded this store may have released
    // on quit). Only a holder may publish.
    let s = repl::poll(&store, &bucket);
    if s.role != Role::Holder {
        repl::acquire(&store, &bucket).expect("acquire lease");
    }

    let panel: i64 = store
        .conn()
        .query_row(READ, [], |r| r.get(0))
        .expect("a compose draft to edit");
    store
        .write(move |tx| tx.execute(EDIT, [panel]).map(|_| ()))
        .expect("edit draft");
    repl::poll(&store, &bucket); // publish the edit as a batch

    let body: String = store
        .conn()
        .query_row(SHOW, [panel], |r| r.get(0))
        .unwrap_or_default();
    println!("reseed-edit: draft {panel} is now {body:?}, published");
}
