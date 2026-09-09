//! Content measurements shared by every file-viewing panel.

use kernel::{caps::Preview, theme};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Measure {
    #[default]
    Empty,
    Text(std::sync::Arc<[usize]>),
    Image(u32, u32),
    Pdf(u32, u32),
}

impl Measure {
    pub fn of(preview: &Preview) -> Self {
        match preview {
            Preview::Text(text) => Self::text(text),
            Preview::Image(bytes) => {
                kernel::caps::image_size(bytes).map_or(Self::Empty, |(w, h)| Self::Image(w, h))
            }
            _ => Self::Empty,
        }
    }

    pub fn text(text: &str) -> Self {
        Self::Text(text.lines().map(|line| line.chars().count()).collect())
    }

    /// `cols` measures a four-unit column. Choose a width from the content,
    /// then measure height at that width; the workspace clamps both to its grid.
    pub fn wish(&self, cols: usize, chrome: usize) -> (u32, u32) {
        let width = match self {
            Self::Image(w, h) | Self::Pdf(w, h) if *w > h.saturating_mul(2) => 8,
            Self::Image(w, h) | Self::Pdf(w, h) if w > h => 6,
            Self::Pdf(..) => 5,
            _ => 4,
        };
        // A continuous document asks for reading room once, independent of
        // which page is visible. Scrolling never resizes the panel.
        if matches!(self, Self::Pdf(..)) { return (width, 6); }
        let cols = cols.max(1).saturating_mul(width as usize) / 4;
        let lines = match self {
            Self::Text(lengths) => lengths
                .iter()
                .map(|n| n.div_ceil(cols.max(1)).max(1))
                .sum::<usize>()
                .max(1),
            Self::Image(w, h) | Self::Pdf(w, h) => (cols as f64 * theme::MONO_ADV * f64::from(*h)
                / f64::from((*w).max(1))
                / theme::LINE_H)
                .ceil()
                .min(120.0) as usize,
            Self::Empty => 0,
        };
        (width, ((chrome + lines).div_ceil(6) as u32).clamp(3, 6))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wishes_follow_the_contents_and_the_reading_width() {
        assert_eq!(Measure::text("short").wish(60, 7), (4, 3));
        assert_eq!(Measure::text(&"line\n".repeat(100)).wish(60, 7), (4, 6));
        assert!(
            Measure::text(&"x".repeat(1000)).wish(20, 7).1
                > Measure::text(&"x".repeat(1000)).wish(120, 7).1
        );
        assert!(Measure::Image(1600, 500).wish(60, 7).0 > Measure::Image(500, 1600).wish(60, 7).0);
        assert!(Measure::Pdf(842, 595).wish(60, 7).0 > Measure::Pdf(595, 842).wish(60, 7).0);
        assert_eq!(Measure::Image(32, 32).wish(20, 7), (4, 3));
    }
}
