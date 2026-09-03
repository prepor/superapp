//! Device-sync test helper: acting as the lease
//! holder, append a marker to the single compose draft and publish it — a
//! peer's edit that a *running* follower must pick up live (its retained
//! compose widget seeds from the row only once, so the shell re-seeds it).
//!
//! `reseed-edit <db> <bucket-url>`

use std::path::Path;

use superapp::object::HttpBucket;
use superapp::repl::{self, Role};
use superapp::store::Store;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (db, url) = (&args[1], &args[2]);
    let store = Store::open(Some(Path::new(db))).expect("open store");
    let bucket = HttpBucket::new(url);

    // Re-adopt the lease (the app that seeded this db may have released on
    // quit). Only a holder may publish.
    let s = repl::poll(&store, &bucket);
    if s.role != Role::Holder {
        repl::acquire(&store, &bucket).expect("acquire lease");
    }

    let panel: i64 = store
        .conn()
        .query_row("SELECT panel FROM draft LIMIT 1", [], |r| r.get(0))
        .expect("a compose draft to edit");
    store
        .write(move |tx| {
            tx.execute(
                "UPDATE draft SET body = body || ' beta' WHERE panel=?1",
                [panel],
            )
            .map(|_| ())
        })
        .expect("edit draft");
    repl::poll(&store, &bucket); // publish the edit as a batch

    let body: String = store
        .conn()
        .query_row("SELECT body FROM draft WHERE panel=?1", [panel], |r| {
            r.get(0)
        })
        .unwrap_or_default();
    println!("reseed-edit: draft panel {panel} is now {body:?}, published");
}
