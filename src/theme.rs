//! The look: black on white, monochrome, colour only for errors.
//!
//! Numbers live here rather than in the DSL so Rust layout code and shader
//! defaults cannot drift apart (mosaic's rule).

/// A colour as makepad wants it: RGBA floats in `[0, 1]`.
pub type Rgba = [f32; 4];

/// Workspace background.
pub const BG: Rgba = [1.0, 1.0, 1.0, 1.0];
/// Primary text and strong borders (near-black).
pub const INK: Rgba = [0.078, 0.078, 0.078, 1.0];
/// Secondary text.
pub const TEXT2: Rgba = [0.353, 0.353, 0.353, 1.0];
/// Muted text (footnotes, placeholders).
pub const MUTED: Rgba = [0.565, 0.565, 0.565, 1.0];
/// Hairline rules.
pub const RULE: Rgba = [0.863, 0.863, 0.863, 1.0];
/// Hover backing.
pub const HOVER: Rgba = [0.937, 0.937, 0.937, 1.0];
/// Selected-row backing.
pub const SEL: Rgba = [0.906, 0.906, 0.906, 1.0];
/// The only non-monochrome colour: errors.
pub const ERR: Rgba = [0.627, 0.082, 0.0, 1.0];

/// Grid columns across the viewport.
pub const GRID_W: u32 = 12;
/// Grid rows down the viewport.
pub const GRID_H: u32 = 6;
/// Gap between panels and viewport edges, in points.
pub const GAP: f64 = 8.0;
/// Panel header height, in points.
pub const HEAD_H: f64 = 26.0;
/// Body text size, in points (renders ≈14 px, the web prototype's 13 px).
pub const FONT_SIZE: f64 = 10.5;
/// Label text size: uppercase tracked labels, headers, buttons.
pub const LABEL_SIZE: f64 = 8.25;
/// Extra tracking between label characters, as a fraction of the advance.
pub const LABEL_TRACK: f64 = 0.18;
/// Panel body horizontal padding, in points.
pub const PAD_X: f64 = 10.0;
/// Panel body vertical padding, in points.
pub const PAD_Y: f64 = 8.0;
/// Button box height, in points.
pub const BTN_H: f64 = 18.0;
