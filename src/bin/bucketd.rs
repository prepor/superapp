//! Local object-store server for the device-sync demo.
//!
//! A tiny HTTP server implementing the compare-and-swap semantics the sync
//! engine needs (see [`superapp::object`]), backed by a directory. It stands
//! in for R2/S3 so a macOS build and an android emulator can share one bucket
//! with no cloud credentials:
//!
//! ```sh
//! superapp-bucketd --dir /tmp/superapp-bucket --port 9000
//! # macOS app:   --bucket http://127.0.0.1:9000
//! # android app: --bucket http://10.0.2.2:9000   (the emulator's host alias)
//! ```
//!
//! Thread-per-connection, one lock serialising the read-modify-write so a CAS
//! is atomic. Not for production — no auth, no TLS — just a demo transport.

use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

fn main() {
    let mut dir = PathBuf::from(
        std::env::var("SUPERAPP_BUCKET_DIR").unwrap_or_else(|_| "/tmp/superapp-bucket".into()),
    );
    let mut port: u16 = 9000;
    let mut bind = "0.0.0.0".to_string();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--dir" => {
                if let Some(d) = args.next() {
                    dir = PathBuf::from(d);
                }
            }
            "--port" => {
                if let Some(p) = args.next().and_then(|s| s.parse().ok()) {
                    port = p;
                }
            }
            "--bind" => {
                if let Some(b) = args.next() {
                    bind = b;
                }
            }
            other => eprintln!("bucketd: ignoring unknown argument {other:?}"),
        }
    }

    std::fs::create_dir_all(&dir).expect("create the bucket directory");
    let listener = TcpListener::bind((bind.as_str(), port)).unwrap_or_else(|e| {
        eprintln!("bucketd: cannot bind {bind}:{port}: {e}");
        std::process::exit(1);
    });
    eprintln!("bucketd: serving {} on http://{bind}:{port}", dir.display());

    // One lock so a compare-and-swap is atomic against concurrent devices.
    let lock = Arc::new(Mutex::new(()));
    for conn in listener.incoming() {
        let Ok(mut stream) = conn else { continue };
        let dir = dir.clone();
        let lock = lock.clone();
        std::thread::spawn(move || {
            let _guard = lock.lock().expect("bucket lock");
            let _ = superapp::object::serve_conn(&dir, &mut stream);
        });
    }
}
