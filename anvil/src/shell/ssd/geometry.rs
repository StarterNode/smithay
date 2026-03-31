use super::HEADER_BAR_HEIGHT;

/// Decoration geometry offsets for SSD windows.
/// Formalizes the space taken by title bar and borders.
///
/// Current approach: borders overlay client content at edges (no client size reduction).
/// Title bar adds to window height above the client surface.
/// Future phases may inset the client by border_width on each side.
#[derive(Debug, Clone, Copy)]
pub struct DecorationGeometry {
    pub title_bar_height: i32,
    pub border_width: i32,
}

impl DecorationGeometry {
    /// Active SSD geometry with the given border width.
    pub fn active(border_width: i32) -> Self {
        Self {
            title_bar_height: HEADER_BAR_HEIGHT,
            border_width,
        }
    }

    /// No decorations (CSD or fullscreen).
    pub fn none() -> Self {
        Self {
            title_bar_height: 0,
            border_width: 0,
        }
    }

    /// Height consumed above the client surface (title bar).
    pub fn top_offset(&self) -> i32 {
        self.title_bar_height
    }
}
