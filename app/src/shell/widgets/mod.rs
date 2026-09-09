//! The shared widgets a panel is built from.
//!
//! The table and card are helpers a panel's widget embeds
//! rather than widgets of their own: the [`table`] over a
//! [`ListState`](kernel::richtable::ListState) the panel instance owns, and
//! the file [`card`] over a [`CardData`](card::CardData) the panel fills.
//! The shared [`viewer`] adds text, image and PDF rendering and measurement.
//! Neither helper holds state that belongs to a panel — the table borrows its list
//! from the instance through `as_any` on every draw and event, and the card
//! is handed its data.
//!
//! Their templates are in [`dsl`], registered by the shell's own
//! `script_mod`. The media kit — a picture, a player, a recording meter, a
//! map — is [`media`], with the map's maths and its fake tiles in [`map`]. An app composes them: the chassis, the filter, the row
//! twins and the completion box are the shell's; the row's content and what
//! a row opens are the app's.

pub mod card;
pub mod form;
pub mod viewer;
pub mod dsl;
pub mod map;
pub mod media;
pub mod reveal;
pub mod suggest;
pub mod source_input;
pub mod table;
