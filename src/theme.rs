//! Shared colours, spacing, and type sizes.

/// A colour as makepad wants it: RGBA floats in `[0, 1]`.
pub type Rgba = [f32; 4];

pub const BG: Rgba = [1.0, 1.0, 1.0, 1.0];
pub const INK: Rgba = [0.078, 0.078, 0.078, 1.0];
pub const TEXT2: Rgba = [0.353, 0.353, 0.353, 1.0];
pub const MUTED: Rgba = [0.565, 0.565, 0.565, 1.0];
pub const RULE: Rgba = [0.863, 0.863, 0.863, 1.0];
pub const HOVER: Rgba = [0.937, 0.937, 0.937, 1.0];
pub const SEL: Rgba = [0.906, 0.906, 0.906, 1.0];
pub const ERR: Rgba = [0.627, 0.082, 0.0, 1.0];

pub const GAP: f64 = 8.0;
pub const HEAD_H: f64 = 26.0;
/// Body text size in points.
pub const FONT_SIZE: f64 = 10.5;
/// One mono character's advance, as a fraction of the font size. The face
/// is measured on the first draw; layout uses this value before then.
pub const MONO_ADV: f64 = 0.8;
/// Line height as a fraction of the font size.
pub const LINE_H: f64 = 2.0;
pub const LABEL_SIZE: f64 = 8.25;
pub const LABEL_TRACK: f64 = 0.18;
pub const PAD_X: f64 = 10.0;
pub const PAD_Y: f64 = 8.0;
pub const BTN_H: f64 = 18.0;
pub const TAB_H: f64 = 24.0;
pub const TAB_GAP: f64 = 4.0;
