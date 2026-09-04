//! Files' Makepad half: one widget per panel kind.
//!
//! Each borrows its instance from the scope and calls the instance's own
//! methods; nothing here keeps state the panel could keep instead. The
//! listing is the shared rich table with a row body of its own and the
//! crumb line, the two fields and the status line around it; the card is
//! the shell's own, filled from what the instance read off the disk.
//!
//! Nothing watches a disk, so both call
//! [`observe`](super::panels::Dir::observe) at the top of every draw and
//! every event: that is where a panel learns where it stands in the join
//! chain and that somebody has written.
//!
//! The templates they are built from are in [`ui`](super::ui).

pub mod card;
pub mod dir;

pub use card::CardPanel;
pub use dir::DirPanel;
