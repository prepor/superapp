//! The desktop binary.
//!
//! One line, because everything above the entry point is in the library:
//! android has no `fn main` to put it in, and the two platforms must boot
//! the same app list and the same shell. See `lib.rs`.

fn main() {
    superapp::run();
}
