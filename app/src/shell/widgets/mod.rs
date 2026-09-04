//! The shared widgets a panel is built from.
//!
//! Two components live here, and both are helpers a panel's widget embeds
//! rather than widgets of their own: the [`table`] over a
//! [`ListState`](kernel::richtable::ListState) the panel instance owns, and
//! the file [`card`] over a [`CardData`](card::CardData) the panel fills.
//! Neither holds state that belongs to a panel — the table borrows its list
//! from the instance through `as_any` on every draw and event, and the card
//! is handed its data.
//!
//! Their templates are in [`dsl`], registered by the shell's own
//! `script_mod`. An app composes them: the chassis, the filter, the row
//! twins and the completion box are the shell's; the row's content and what
//! a row opens are the app's.

pub mod card;
pub mod dsl;
pub mod suggest;
pub mod table;
